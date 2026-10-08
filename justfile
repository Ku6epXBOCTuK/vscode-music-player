# Run the backend (release build: debug FLAC encoding is too slow)
# The recipe ignores the exit code: Ctrl+C interrupts just itself (exit 512)
backend: backend-build
    -./backend/target/release/backend.exe

# Build the backend in release mode
backend-build:
    cargo build --release --manifest-path backend/Cargo.toml

# Decode an audio file and print stats (stage 1 check)
decode FILE="testdata/sample.mp3":
    cargo run --manifest-path backend/Cargo.toml -- decode {{FILE}}
