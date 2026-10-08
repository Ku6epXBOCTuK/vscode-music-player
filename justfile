# Run the backend (config.json is created next to the exe in target/debug)
backend:
    cargo run --manifest-path backend/Cargo.toml

# Build the backend in release mode
backend-build:
    cargo build --release --manifest-path backend/Cargo.toml
