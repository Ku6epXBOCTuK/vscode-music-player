import * as vscode from "vscode";
import { spawn } from "node:child_process";
import { appendFileSync, existsSync } from "node:fs";
import * as path from "node:path";

const POLL_INTERVAL_MS = 3000;
const VOLUME_STEP = 0.1;

const out = vscode.window.createOutputChannel("Music Player");
let logFile: string | undefined;

function log(message: string): void {
	const line = `${new Date().toISOString()} ${message}`;
	out.appendLine(line);
	if (logFile) {
		try {
			appendFileSync(logFile, line + "\n");
		} catch {
			// logging must never break the extension
		}
	}
}

function baseUrl(): string {
	const port = vscode.workspace
		.getConfiguration("music-player")
		.get<number>("port", 45880);
	return `http://127.0.0.1:${port}`;
}

interface NowPlaying {
	status: "playing" | "paused" | "stopped";
	track: string | null;
	volume: number;
	playlist_index: number;
}

let trackItem: vscode.StatusBarItem;
let toggleItem: vscode.StatusBarItem;
let nextItem: vscode.StatusBarItem;
let volDownItem: vscode.StatusBarItem;
let volumeItem: vscode.StatusBarItem;
let volUpItem: vscode.StatusBarItem;
let stopItem: vscode.StatusBarItem;
let lastState: NowPlaying | undefined;
let timer: ReturnType<typeof setInterval> | undefined;

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
		// backend offline
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

// Locates the backend binary. Order: explicit setting, repo root next to
// the extension itself (development), opened workspace folders.
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

let backendStartAttempted = false;

// Starts the backend detached (it must survive VS Code closing).
// Silent by design: the poll loop reflects the outcome in the status bar.
function startBackendOnce(extensionPath: string): void {
	if (backendStartAttempted) {
		return;
	}
	const backend = findBackend(extensionPath);
	if (!backend) {
		return;
	}
	backendStartAttempted = true;
	try {
		const child = spawn(backend.exe, [], {
			detached: true,
			stdio: "ignore",
			cwd: backend.cwd,
		});
		child.unref();
		log(` started backend: ${backend.exe}`);
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
		trackItem.tooltip = "music server is not running (press play to start it)";
		toggleItem.text = "$(play)";
		toggleItem.tooltip = "Start music server";
		return;
	}

	if (!wasOnline) {
		log("[music-player] backend is online");
	}
	trackItem.text = `$(music) ${lastState.track ?? "-"}`;
	trackItem.tooltip = "music-player: open settings";
	toggleItem.text =
		lastState.status === "playing" ? "$(debug-pause)" : "$(play)";
	toggleItem.tooltip = lastState.status === "playing" ? "Pause" : "Play";
	volumeItem.text = `${Math.round(lastState.volume * 100)}%`;
	volumeItem.tooltip = "Set volume (exact value)";
}

function setControlsVisible(online: boolean): void {
	for (const item of [nextItem, volDownItem, volumeItem, volUpItem, stopItem]) {
		if (online) {
			item.show();
		} else {
			item.hide();
		}
	}
}

// Never let an exception escape into the extension host
async function safeRefresh(): Promise<void> {
	try {
		await refresh();
	} catch {
		trackItem.text = "$(error) Music off";
		trackItem.tooltip = "music server is not running";
	}
}

async function control(action: string): Promise<void> {
	if (await post("/api/control", { action })) {
		setTimeout(safeRefresh, 250);
	} else {
		vscode.window.showWarningMessage("music server is not reachable");
	}
}

async function setVolume(volume: number): Promise<void> {
	const clamped = Math.min(1, Math.max(0, volume));
	if (await post("/api/volume", { volume: clamped })) {
		setTimeout(safeRefresh, 250);
	}
}

async function stepVolume(delta: number): Promise<void> {
	if (!lastState) {
		vscode.window.showWarningMessage("music server is not reachable");
		return;
	}
	await setVolume(Math.round((lastState.volume + delta) * 100) / 100);
}

function makeItem(priority: number): vscode.StatusBarItem {
	return vscode.window.createStatusBarItem(
		vscode.StatusBarAlignment.Left,
		priority,
	);
}

export function activate(context: vscode.ExtensionContext): void {
	logFile = path.join(context.extensionPath, "music-player.log");
	try {
		log("[music-player] extension activated");
		trackItem = makeItem(100);
		trackItem.command = "music-player.openSettings";

		stopItem = makeItem(99);
		stopItem.text = "$(debug-stop)";
		stopItem.tooltip = "Stop music server";
		stopItem.command = "music-player.stopBackend";

		toggleItem = makeItem(98);
		toggleItem.command = "music-player.toggle";

		nextItem = makeItem(97);
		nextItem.text = "$(chevron-right)";
		nextItem.tooltip = "Next playlist";
		nextItem.command = "music-player.nextPlaylist";

		volDownItem = makeItem(96);
		volDownItem.text = "$(remove)";
		volDownItem.tooltip = "Volume down";
		volDownItem.command = "music-player.volumeDown";

		volumeItem = makeItem(95);
		volumeItem.command = "music-player.setVolume";

		volUpItem = makeItem(94);
		volUpItem.text = "$(add)";
		volUpItem.tooltip = "Volume up";
		volUpItem.command = "music-player.volumeUp";

		for (const item of [
			trackItem,
			toggleItem,
			nextItem,
			volDownItem,
			volumeItem,
			volUpItem,
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
			vscode.commands.registerCommand("music-player.prevPlaylist", () =>
				control("prev_playlist"),
			),
			vscode.commands.registerCommand("music-player.volumeUp", () =>
				stepVolume(VOLUME_STEP),
			),
			vscode.commands.registerCommand("music-player.volumeDown", () =>
				stepVolume(-VOLUME_STEP),
			),
			vscode.commands.registerCommand("music-player.setVolume", async () => {
				const input = await vscode.window.showInputBox({
					prompt: "Volume, 0 to 100",
					value: lastState ? String(Math.round(lastState.volume * 100)) : "80",
					validateInput: (v) => {
						const n = Number(v);
						return Number.isInteger(n) && n >= 0 && n <= 100
							? undefined
							: "enter an integer between 0 and 100";
					},
				});
				if (input !== undefined) {
					await setVolume(Number(input) / 100);
				}
			}),
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
