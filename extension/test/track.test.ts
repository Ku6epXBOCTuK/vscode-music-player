import {
	type NowPlaying,
	cleanTrackName,
	fullTitle,
	statusText,
	trackLabel,
} from "../src/track";

let failed = 0;

function check(name: string, actual: string, expected: string): void {
	if (actual === expected) {
		console.log(`ok   ${name}`);
		return;
	}
	failed++;
	console.error(`FAIL ${name}\n  expected: ${expected}\n  actual:   ${actual}`);
}

function state(overrides: Partial<NowPlaying>): NowPlaying {
	return {
		status: "playing",
		track: null,
		volume: 0.8,
		playlist_index: 0,
		playlist: null,
		playlist_type: null,
		shuffle: false,
		...overrides,
	};
}

const radio = state({
	track: "laut.fm",
	playlist: "laut.fm",
	playlist_type: "network_radio",
});
const local = state({
	track: "01 - track.mp3",
	playlist: "Test Data",
	playlist_type: "local_folder",
});

check("radio keeps the dot in the station name", trackLabel(radio), "laut.fm");
check("radio title is the station name", fullTitle(radio), "laut.fm");
check(
	"radio status shows the station name",
	statusText(radio),
	"$(music) laut.fm",
);
check(
	"radio station name with a leading digit is untouched",
	trackLabel(state({ track: "3FM", playlist_type: "network_radio" })),
	"3FM",
);
check(
	"radio station name with a dot is not shortened",
	trackLabel(
		state({ track: "radio.example.org", playlist_type: "network_radio" }),
	),
	"radio.example.org",
);
check(
	"local file loses the extension",
	cleanTrackName("01 - track.mp3"),
	"track",
);
check("local track label is cleaned", trackLabel(local), "track");
check("local title keeps the folder", fullTitle(local), "Test Data - track");
check(
	"local status keeps the folder",
	statusText(local),
	"$(music) Test Data - track",
);
check("no track shows a dash", trackLabel(state({})), "-");

if (failed > 0) {
	console.error(`${failed} check(s) failed`);
	process.exit(1);
}
console.log("all checks passed");
