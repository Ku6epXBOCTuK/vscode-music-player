export interface NowPlaying {
	status: "playing" | "paused" | "stopped";
	track: string | null;
	volume: number;
	playlist_index: number;
	playlist: string | null;
	playlist_type: string | null;
	shuffle: boolean;
}

export const MAX_ALBUM_LENGTH = 20;
export const MAX_TRACK_LENGTH = 30;

export function truncate(text: string, max: number): string {
	if (text.length <= max) {
		return text;
	}
	return `${text.slice(0, max - 3).trimEnd()}...`;
}

export function cleanTrackName(raw: string): string {
	const withoutExtension = raw.replace(/\.[^.]+$/, "");
	const withoutNumber = withoutExtension.replace(/^\d{1,3}[\s._-]+/, "");
	return withoutNumber.trim() || withoutExtension.trim();
}

export function albumOf(state: NowPlaying): string | undefined {
	return state.playlist_type === "local_folder" && state.playlist
		? state.playlist
		: undefined;
}

// NOTE: A radio track is the station name ("laut.fm"), a local track is a
// file name ("01 - track.mp3"). Only file names get the extension and the
// leading track number stripped.
export function isFileTrack(state: NowPlaying): boolean {
	return state.playlist_type === "local_folder";
}

export function trackLabel(state: NowPlaying): string {
	if (!state.track) {
		return "-";
	}
	return isFileTrack(state) ? cleanTrackName(state.track) : state.track;
}

export function fullTitle(state: NowPlaying): string {
	const track = trackLabel(state);
	const album = albumOf(state);
	return album ? `${album} - ${track}` : track;
}

export function statusText(state: NowPlaying): string {
	const track = truncate(trackLabel(state), MAX_TRACK_LENGTH);
	const album = albumOf(state);
	return album
		? `$(music) ${truncate(album, MAX_ALBUM_LENGTH)} - ${track}`
		: `$(music) ${track}`;
}
