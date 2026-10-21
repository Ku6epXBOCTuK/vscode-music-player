const esbuild = require("esbuild");
const { spawnSync } = require("node:child_process");

async function main() {
	await esbuild.build({
		entryPoints: ["test/track.test.ts"],
		bundle: true,
		format: "cjs",
		platform: "node",
		outfile: "dist/track.test.js",
		logLevel: "silent",
	});
	const result = spawnSync(process.execPath, ["dist/track.test.js"], {
		stdio: "inherit",
	});
	process.exit(result.status ?? 1);
}

main().catch((e) => {
	console.error(e);
	process.exit(1);
});
