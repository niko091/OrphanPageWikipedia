#!/bin/sh
# Run from the repository root. The heroku/nodejs buildpack runs this script
# through the heroku-build npm script instead of npm run build.
# Toolforge runs heroku/nodejs before emk/rust, so Cargo is not available yet.
# This script installs a temporary Rust toolchain for the WebAssembly build.
# The emk/rust buildpack then builds the server binary with its own toolchain.
set -eu
bindgen_version=0.2.105
tools=$(mktemp -d)
trap 'rm -rf "$tools"' EXIT HUP INT TERM
if ! command -v cargo >/dev/null 2>&1; then
    # Keep the toolchain outside the app directory, so it is not in the image.
    export RUSTUP_HOME="$tools/rustup" CARGO_HOME="$tools/cargo"
    curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
        https://sh.rustup.rs --output "$tools/rustup-init.sh"
    sh "$tools/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain stable
    PATH="$CARGO_HOME/bin:$PATH"
fi
rustup target add wasm32-unknown-unknown
if [ -z "${WASM_BINDGEN:-}" ]; then
    cargo install wasm-bindgen-cli --version "$bindgen_version" --locked --root "$tools/wasm-bindgen"
    export WASM_BINDGEN="$tools/wasm-bindgen/bin/wasm-bindgen"
fi
# build.rs needs this archive when emk/rust runs cargo build.
sh scripts/prepare-fasttext.sh
# build-web.ts reads the WebAssembly module from target/.
CARGO_TARGET_DIR="$PWD/target" npm run build
