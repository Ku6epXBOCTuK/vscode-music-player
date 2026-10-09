# Show available recipes
default:
    @just --list

# Run the backend (release build: debug FLAC encoding is too slow)
# The recipe ignores the exit code: Ctrl+C interrupts just itself (exit 512)
backend: backend-build
    -./backend/target/release/backend.exe

# Build the backend in release mode
backend-build:
    cargo build --release --manifest-path backend/Cargo.toml

# Lint with clippy
check:
    cargo clippy --manifest-path backend/Cargo.toml --release

# Decode an audio file and print stats (stage 1 check)
decode FILE="testdata/sample.mp3":
    cargo run --manifest-path backend/Cargo.toml -- decode {{FILE}}

# Check the backend API is alive
health:
    curl -s http://127.0.0.1:45880/api/health

# Show current playback state (track, status, volume)
status:
    curl -s http://127.0.0.1:45880/api/now_playing

# Send a control action: play / pause / stop / next_playlist / prev_playlist
control ACTION="play":
    curl -s -X POST http://127.0.0.1:45880/api/control -H "Content-Type: application/json" -d "{\"action\":\"{{ACTION}}\"}"

# Set volume, 0.0 to 1.0
volume LEVEL="0.5":
    curl -s -X POST http://127.0.0.1:45880/api/volume -H "Content-Type: application/json" -d "{\"volume\":{{LEVEL}}}"

# Capture N seconds of /stream and validate the bytes with ffmpeg
# curl exits 28 on --max-time by design (the stream is endless), ignore it
check-stream SECONDS="10":
    -curl -s -o testdata/stream-check.flac --max-time {{SECONDS}} http://127.0.0.1:45880/stream
    ffmpeg -v error -i testdata/stream-check.flac -f null -
    @echo stream-check OK, capture kept at testdata/stream-check.flac

# Install workspace dependencies (root + extension)
ext-setup:
    pnpm install

# Build the VS Code extension
ext-build:
    pnpm --filter ./extension run compile

# Package the extension into a .vsix file
ext-package: ext-build
    pnpm --filter ./extension exec vsce package
