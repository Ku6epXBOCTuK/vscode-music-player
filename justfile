# Run the backend (release build: debug FLAC encoding is too slow)
backend:
    cargo run --release --manifest-path backend/Cargo.toml

# Build the backend in release mode
backend-build:
    cargo build --release --manifest-path backend/Cargo.toml

# Decode an audio file and print stats (stage 1 check)
decode FILE="testdata/sample.mp3":
    cargo run --manifest-path backend/Cargo.toml -- decode {{FILE}}
