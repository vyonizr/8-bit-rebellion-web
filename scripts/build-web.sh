#!/bin/sh
# Builds touchHLE as WebAssembly inside the toolchain container and copies the
# result to web/dist/. Usage: scripts/build-web.sh [debug|release]
set -eu
profile="${1:-release}"
root="$(cd "$(dirname "$0")/.." && pwd)"
docker build -q -t 8br-toolchain "$root/docker" >/dev/null
flag=""; [ "$profile" = release ] && flag="--release"
# Named volumes keep the cargo registry and target dir out of the slow
# Windows bind mount.
MSYS_NO_PATHCONV=1 docker run --rm \
  -v "$root:/src" -v 8br-cargo-registry:/opt/cargo/registry -v 8br-target:/target \
  -e CARGO_TARGET_DIR=/target -e TOUCHHLE_EMCC_FLAGS="${TOUCHHLE_EMCC_FLAGS:-}" \
  -w /src/touchHLE 8br-toolchain \
  sh -c "cargo build $flag --target wasm32-unknown-emscripten --no-default-features --bin touchHLE \
    && mkdir -p /src/web/dist && cp /target/wasm32-unknown-emscripten/$profile/touchHLE.js /target/wasm32-unknown-emscripten/$profile/touchHLE.wasm /target/wasm32-unknown-emscripten/$profile/deps/touchHLE.data /src/web/dist/"
