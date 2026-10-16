# Toolchain for building touchHLE as WebAssembly.
FROM emscripten/emsdk:6.0.10

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential pkg-config cmake \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.1 \
      --target wasm32-unknown-emscripten \
 && chmod -R a+w /opt/rustup /opt/cargo
