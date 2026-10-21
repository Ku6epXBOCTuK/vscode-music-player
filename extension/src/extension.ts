import { spawn } from "node:child_process";
import {
	appendFileSync,
	closeSync,
	existsSync,
	fstatSync,
	openSync,
	readSync,
} from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

const POLL_INTERVAL_MS = 3000;
const VOLUME_STEP_PERCENT = 10;
const MAX_ALBUM_LENGTH = 20;
const MAX_TRACK_LENGTH = 30;

const out = vscode.window.createOutputChannel("Music Player");
let logFile: string | undefined;

function log(message: string): void {
	const line = `${new Date().toISOString()} ${message}`;
	out.appendLine(line);
	if (logFile) {
		try {
			appendFileSync(logFile, line + "\n");
		} catch {
			// NOTE: logging must never break the extension
		}
	}
}

function backendPort(): number {
	return vscode.workspace
		.getConfiguration("music-player")
		.get<number>("port", 45880);
}

function baseUrl(): string {
	return `http://127.0.0.1:${backendPort()}`;
}

interface NowPlaying {
	status: "playing" | "paused" | "stopped";
	track: string | null;
	volume: number;
	playlist_index: number;
	playlist: string | null;
	playlist_type: string | null;
	shuffle: boolean;
}

let trackItem: vscode.StatusBarItem;
let toggleItem: vscode.StatusBarItem;
let nextItem: vscode.StatusBarItem;
let nextTrackItem: vscode.StatusBarItem;
let shuffleItem: vscode.StatusBarItem;
let volumeItem: vscode.StatusBarItem;
let stopItem: vscode.StatusBarItem;
let lastState: NowPlaying | undefined;
let timer: ReturnType<typeof setInterval> | undefined;

function truncate(text: string, max: number): string {
	if (text.length <= max) {
		return text;
	}
	return `${text.slice(0, max - 3).trimEnd()}...`;
}

function cleanTrackName(raw: string): string {
	const withoutExtension = raw.replace(/\.[^.]+$/, "");
	const withoutNumber = withoutExtension.replace(/^\d{1,3}[\s._-]+/, "");
	return withoutNumber.trim() || withoutExtension.trim();
}

function albumOf(state: NowPlaying): string | undefined {
	return state.playlist_type === "local_folder" && state.playlist
		? state.playlist
		: undefined;
}

function fullTitle(state: NowPlaying): string {
	const track = state.track ? cleanTrackName(state.track) : "-";
	const album = albumOf(state);
	return album ? `${album} - ${track}` : track;
}

function statusText(state: NowPlaying): string {
	const track = truncate(
		state.track ? cleanTrackName(state.track) : "-",
		MAX_TRACK_LENGTH,
	);
	const album = albumOf(state);
	return album
		? `$(music) ${truncate(album, MAX_ALBUM_LENGTH)} - ${track}`
		: `$(music) ${track}`;
}

async function post(path: string, body: unknown): Promise<boolean> {
	try {
		const res = await fetch(`${baseUrl()}${path}`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		});
		if (!res.ok) {
			log(` POST ${path} -> HTTP ${res.status}`);
		}
		return res.ok;
	} catch (e) {
		log(` POST ${path} failed: ${e}`);
		return false;
	}
}

async function getNowPlaying(): Promise<NowPlaying | undefined> {
	try {
		const res = await fetch(`${baseUrl()}/api/now_playing`);
		if (res.ok) {
			return (await res.json()) as NowPlaying;
		}
	} catch {
		// NOTE: backend offline
	}
	return undefined;
}

async function isBackendUp(): Promise<boolean> {
	try {
		const res = await fetch(`${baseUrl()}/api/health`);
		return res.ok;
	} catch {
		return false;
	}
}

// NOTE: Locates the backend binary. Order: explicit setting, repo root
// next to the extension itself (development), opened workspace folders.
// Returns the exe path and the cwd it must run in.
function findBackend(
	extensionPath: string,
): { exe: string; cwd: string } | undefined {
	const configured = vscode.workspace
		.getConfiguration("music-player")
		.get<string>("backendPath", "")
		.trim();
	if (configured) {
		if (existsSync(configured)) {
			return { exe: configured, cwd: path.dirname(configured) };
		}
		log(` configured backendPath does not exist: ${configured}`);
		return undefined;
	}

	const exeName = process.platform === "win32" ? "backend.exe" : "backend";
	const rel = path.join("backend", "target", "release", exeName);
	const candidates: { exe: string; cwd: string }[] = [
		{
			exe: path.resolve(extensionPath, "..", rel),
			cwd: path.resolve(extensionPath, ".."),
		},
	];
	for (const folder of vscode.workspace.workspaceFolders ?? []) {
		candidates.push({
			exe: path.join(folder.uri.fsPath, rel),
			cwd: folder.uri.fsPath,
		});
	}
	for (const c of candidates) {
		if (existsSync(c.exe)) {
			return c;
		}
	}
	log(
		`[music-player] backend exe not found; checked: ${candidates.map((c) => c.exe).join(", ")}`,
	);
	return undefined;
}

// NOTE: Reads the last lines of a file without loading it entirely.
function tailFile(file: string, maxLines = 12): string[] {
	try {
		if (!existsSync(file)) {
			return [];
		}
		const fd = openSync(file, "r");
		try {
			const size = fstatSync(fd).size;
			const length = Math.min(size, 16384);
			const buffer = Buffer.alloc(length);
			readSync(fd, buffer, 0, length, size - length);
			return buffer
				.toString("utf8")
				.split(/\r?\n/)
				.filter((line) => line.trim().length > 0)
				.slice(-maxLines);
		} finally {
			closeSync(fd);
		}
	} catch {
		return [];
	}
}

// NOTE: The backend writes its own log next to the executable; the stderr
// file catches the startup failures that happen before the log is opened.
function backendLogTail(exeDir: string): string[] {
	return [
		...tailFile(path.join(exeDir, "backend-stderr.log")),
		...tailFile(path.join(exeDir, "music-player.log")),
	];
}

let backendStartAttempted = false;

// NOTE: Starts the backend detached (it must survive VS Code closing).
// The port comes from the extension settings, so the backend and the
// extension always agree on it.
function startBackendOnce(extensionPath: string): void {
	if (backendStartAttempted) {
		return;
	}
	const backend = findBackend(extensionPath);
	if (!backend) {
		return;
	}
	backendStartAttempted = true;
	const exeDir = path.dirname(backend.exe);
	const stderrPath = path.join(exeDir, "backend-stderr.log");
	let stderrFd: number | undefined;
	try {
		stderrFd = openSync(stderrPath, "a");
	} catch {
		stderrFd = undefined;
	}
	try {
		const child = spawn(backend.exe, ["--port", String(backendPort())], {
			detached: true,
			stdio: ["ignore", "ignore", stderrFd ?? "ignore"],
			cwd: backend.cwd,
		});
		if (stderrFd !== undefined) {
			closeSync(stderrFd);
		}
		child.unref();
		log(` started backend: ${backend.exe} (port ${backendPort()})`);
	} catch (e) {
		log(` failed to start backend: ${e}`);
	}
}

async function ensureBackend(extensionPath: string): Promise<void> {
	if (await isBackendUp()) {
		log("[music-player] backend already running");
		return;
	}
	log("[music-player] backend is down");
	const backend = findBackend(extensionPath);
	if (!backend) {
		return;
	}
	const tail = backendLogTail(path.dirname(backend.exe));
	if (tail.length === 0) {
		return;
	}
	for (const line of tail) {
		log(` backend: ${line}`);
	}
	const last = tail[tail.length - 1];
	vscode.window.showWarningMessage(
		`Music server is not running. Last backend message: ${last}`,
	);
}

async function refresh(): Promise<void> {
	const wasOnline = !!lastState;
	lastState = await getNowPlaying();
	setControlsVisible(!!lastState);

	if (!lastState) {
		if (wasOnline) {
			log("[music-player] backend went offline");
		}
		backendStartAttempted = false;
		trackItem.text = "$(error) Music off";
		trackItem.tooltip = "Music server is not running";
		toggleItem.text = "$(play)";
		toggleItem.tooltip = "Start music server";
		return;
	}

	if (!wasOnline) {
		log("[music-player] backend is online");
	}
	trackItem.text = statusText(lastState);
	trackItem.tooltip = `Music-player: ${fullTitle(lastState)} (click to open settings)`;
	toggleItem.text =
		lastState.status === "playing" ? "$(debug-pause)" : "$(play)";
	toggleItem.tooltip = lastState.status === "playing" ? "Pause" : "Play";
	volumeItem.text = `${Math.round(lastState.volume * 100)}%`;
	volumeItem.tooltip = "Set volume (0-100, step 10)";
	nextTrackItem.tooltip = "Next track";
	nextItem.tooltip = "Next playlist";
	shuffleItem.text = lastState.shuffle ? "$(arrow-swap)" : "$(list-ordered)";
	shuffleItem.tooltip = lastState.shuffle
		? "Turn shuffle off"
		: "Turn shuffle on";
}

function setControlsVisible(online: boolean): void {
	for (const item of [
		nextItem,
		nextTrackItem,
		shuffleItem,
		volumeItem,
		stopItem,
	]) {
		if (online) {
			item.show();
		} else {
			item.hide();
		}
	}
}

// NOTE: Never let an exception escape into the extension host
async function safeRefresh(): Promise<void> {
	try {
		await refresh();
	} catch {
		trackItem.text = "$(error) Music off";
		trackItem.tooltip = "Music server is not running";
	}
}

async function control(action: string): Promise<void> {
	if (await post("/api/control", { action })) {
		setTimeout(safeRefresh, 250);
	} else {
		vscode.window.showWarningMessage("Music server is not reachable");
	}
}

async function setVolume(volume: number): Promise<void> {
	const clamped = Math.min(1, Math.max(0, volume));
	if (await post("/api/volume", { volume: clamped })) {
		setTimeout(safeRefresh, 250);
	}
}

async function pickVolume(): Promise<void> {
	const current = lastState ? Math.round(lastState.volume * 100) : -1;
	const choices: vscode.QuickPickItem[] = [];
	for (let percent = 0; percent <= 100; percent += VOLUME_STEP_PERCENT) {
		choices.push({
			label: `${percent}%`,
			description: percent === current ? "current" : undefined,
		});
	}
	const picked = await vscode.window.showQuickPick(choices, {
		placeHolder: "Volume (0-100)",
	});
	if (picked) {
		await setVolume(Number.parseInt(picked.label, 10) / 100);
	}
}

function makeItem(priority: number): vscode.StatusBarItem {
	return vscode.window.createStatusBarItem(
		vscode.StatusBarAlignment.Right,
		priority,
	);
}

export function activate(context: vscode.ExtensionContext): void {
	logFile = path.join(context.extensionPath, "music-player.log");
	try {
		log("[music-player] extension activated");
		trackItem = makeItem(50);
		trackItem.command = "music-player.openSettings";

		stopItem = makeItem(49);
		stopItem.text = "$(debug-stop)";
		stopItem.tooltip = "Stop music server";
		stopItem.command = "music-player.stopBackend";

		toggleItem = makeItem(48);
		toggleItem.command = "music-player.toggle";

		nextTrackItem = makeItem(47);
		nextTrackItem.text = "$(chevron-right)";
		nextTrackItem.tooltip = "Next track";
		nextTrackItem.command = "music-player.nextTrack";

		nextItem = makeItem(46);
		nextItem.text = "$(library)";
		nextItem.tooltip = "Next playlist";
		nextItem.command = "music-player.nextPlaylist";

		shuffleItem = makeItem(45);
		shuffleItem.command = "music-player.toggleShuffle";

		volumeItem = makeItem(44);
		volumeItem.command = "music-player.setVolume";

		for (const item of [
			trackItem,
			toggleItem,
			nextItem,
			nextTrackItem,
			shuffleItem,
			volumeItem,
			stopItem,
		]) {
			context.subscriptions.push(item);
		}
		trackItem.show();
		toggleItem.show();
		setControlsVisible(false);

		context.subscriptions.push(
			vscode.commands.registerCommand("music-player.toggle", () => {
				if (!lastState) {
					log("[music-player] play pressed while offline, starting backend");
					startBackendOnce(context.extensionPath);
					return;
				}
				control(lastState.status === "playing" ? "pause" : "play");
			}),
			vscode.commands.registerCommand("music-player.stopBackend", async () => {
				log("[music-player] stop backend requested");
				const ok = await post("/api/shutdown", {});
				log(` shutdown POST result: ${ok}`);
				setTimeout(safeRefresh, 500);
			}),
			vscode.commands.registerCommand("music-player.nextPlaylist", () =>
				control("next_playlist"),
			),
			vscode.commands.registerCommand("music-player.nextTrack", () =>
				control("next_track"),
			),
			vscode.commands.registerCommand("music-player.toggleShuffle", () =>
				control("toggle_shuffle"),
			),
			vscode.commands.registerCommand("music-player.prevPlaylist", () =>
				control("prev_playlist"),
			),
			vscode.commands.registerCommand("music-player.setVolume", () =>
				pickVolume(),
			),
			vscode.commands.registerCommand("music-player.openSettings", () =>
				vscode.env.openExternal(vscode.Uri.parse(baseUrl())),
			),
		);

		timer = setInterval(safeRefresh, POLL_INTERVAL_MS);
		void ensureBackend(context.extensionPath).then(() => safeRefresh());
	} catch (e) {
		log(`[music-player] ACTIVATION FAILED: ${e}`);
		vscode.window.showErrorMessage(`music-player failed to activate: ${e}`);
		throw e;
	}
}

export function deactivate(): void {
	if (timer) {
		clearInterval(timer);
	}
}
