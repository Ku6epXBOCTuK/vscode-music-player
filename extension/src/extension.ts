import * as vscode from "vscode";

const BASE_URL = "http://127.0.0.1:3000";
const POLL_INTERVAL_MS = 3000;
const VOLUME_STEP = 0.1;

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
let lastState: NowPlaying | undefined;
let timer: ReturnType<typeof setInterval> | undefined;

async function post(path: string, body: unknown): Promise<boolean> {
	try {
		const res = await fetch(`${BASE_URL}${path}`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		});
		return res.ok;
	} catch {
		return false;
	}
}

async function getNowPlaying(): Promise<NowPlaying | undefined> {
	try {
		const res = await fetch(`${BASE_URL}/api/now_playing`);
		if (res.ok) {
			return (await res.json()) as NowPlaying;
		}
	} catch {
		// backend offline
	}
	return undefined;
}

async function refresh(): Promise<void> {
	lastState = await getNowPlaying();

	if (!lastState) {
		trackItem.text = "$(error) backend off";
		trackItem.tooltip = "music-player backend is not running";
		toggleItem.text = "$(play)";
		volumeItem.text = "$(mute)";
		return;
	}

	trackItem.text = `$(music) ${lastState.track ?? "-"}`;
	trackItem.tooltip = "music-player: open settings";
	toggleItem.text =
		lastState.status === "playing" ? "$(debug-pause)" : "$(play)";
	toggleItem.tooltip = lastState.status === "playing" ? "Pause" : "Play";
	volumeItem.text = `${Math.round(lastState.volume * 100)}%`;
	volumeItem.tooltip = "Set volume (exact value)";
}

// Never let an exception escape into the extension host
async function safeRefresh(): Promise<void> {
	try {
		await refresh();
	} catch {
		trackItem.text = "$(error) backend off";
		trackItem.tooltip = "music-player backend is not running";
	}
}

async function control(action: string): Promise<void> {
	if (await post("/api/control", { action })) {
		setTimeout(safeRefresh, 250);
	} else {
		vscode.window.showWarningMessage("music-player backend is not reachable");
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
		vscode.window.showWarningMessage("music-player backend is not reachable");
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
	trackItem = makeItem(100);
	trackItem.command = "music-player.openSettings";

	toggleItem = makeItem(99);
	toggleItem.command = "music-player.toggle";

	nextItem = makeItem(98);
	nextItem.text = "$(chevron-right)";
	nextItem.tooltip = "Next playlist";
	nextItem.command = "music-player.nextPlaylist";

	volDownItem = makeItem(97);
	volDownItem.text = "$(remove)";
	volDownItem.tooltip = "Volume down";
	volDownItem.command = "music-player.volumeDown";

	volumeItem = makeItem(96);
	volumeItem.command = "music-player.setVolume";

	volUpItem = makeItem(95);
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
	]) {
		item.show();
		context.subscriptions.push(item);
	}

	context.subscriptions.push(
		vscode.commands.registerCommand("music-player.toggle", () =>
			control(lastState?.status === "playing" ? "pause" : "play"),
		),
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
			vscode.env.openExternal(vscode.Uri.parse(BASE_URL)),
		),
	);

	timer = setInterval(safeRefresh, POLL_INTERVAL_MS);
	safeRefresh();
}

export function deactivate(): void {
	if (timer) {
		clearInterval(timer);
	}
}
