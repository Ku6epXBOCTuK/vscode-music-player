# Distribution plan

Future goal: let other people install the player with one click. Sources are
open, so the trusted model is: binaries are built by GitHub Actions, and the
extension downloads the backend on first run (the rust-analyzer model). No exe
bundled in the .vsix.

## Phase 1: Reproducible builds (GitHub Actions)

- [ ] Workflow `.github/workflows/release.yml`, trigger: git tag `v*`
- [ ] Matrix build of the backend: `windows-latest`, `ubuntu-latest`,
      `macos-latest` (targets x86_64; later aarch64 for mac/linux)
- [ ] Artifacts: `music-player-backend-<os>-<arch>.zip` (exe + default config +
      README), plus `SHA256SUMS.txt` with checksums
- [ ] Workflow builds the extension too (`pnpm install`, `just ext-package`) and
      attaches the `.vsix` to the GitHub Release
- [ ] License inventory of bundled crates (symphonia/flacenc/rubato are
      MIT/Apache — fine to redistribute) in `THIRD_PARTY_LICENSES.md`

## Phase 2: Download-on-first-run in the extension

- [ ] Extension command "Music Player: Install/Update Music Server"
- [ ] On activate (or on first Play): if backend missing, download the release
      zip for the current platform from
      `https://github.com/<owner>/music-player/releases/latest/download/...`
- [ ] Verify SHA-256 against `SHA256SUMS.txt` before running anything
- [ ] Install location: `~/.music-player/` (per-user, no admin needed);
      `music-player.backendPath` setting points there automatically
- [ ] Show progress notification; on failure keep the current silent "Music off"
      state and log to the Music Player output channel

## Phase 3: Publishing

- [ ] GitHub repo public, README with screenshots/OBS setup guide, LICENSE
- [ ] Marketplace: create publisher (Azure DevOps PAT), `vsce publish`
- [ ] Open VSX: `ovsx publish` (for VSCodium and portable builds)
- [ ] Marketplace policy notes: no bundled exe = almost nothing to scan; README
      must state that the extension downloads a binary on first run and from
      where (transparency requirement)
- [ ] Optional later: per-platform vsix with bundled exe
      (`vsce package --target win32-x64`) for offline installs

## Notes

- Current state for personal use: `just ext-package` produces
  `extension/music-player-extension-<version>.vsix`; the backend path is set
  manually via the `music-player.backendPath` setting.
- Default port is 45880 (both sides configurable).
