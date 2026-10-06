# Builds the Linux packages for both architectures from one machine. Zig is
# the C compiler and linker: it targets a chosen glibc version, so a binary
# built here runs on distributions older than this image.
FROM rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3-pip dpkg-dev \
 && rm -rf /var/lib/apt/lists/* \
 && pip3 install --break-system-packages ziglang==0.13.0 \
 && rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
 && cargo install --locked cargo-zigbuild cargo-deb \
 && rm -rf /usr/local/cargo/registry
