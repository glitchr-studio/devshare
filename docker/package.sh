#!/bin/sh
# Run inside the build image: one .deb and one tarball per architecture, in dist/.
set -eu

# The oldest glibc the binaries accept: Debian 10, Ubuntu 20.04, RHEL 8.
GLIBC=2.28
version=$(cargo metadata --no-deps --format-version 1 | sed 's/.*"name":"devshare-cli","version":"\([^"]*\)".*/\1/')
mkdir -p dist

for arch in x86_64:amd64 aarch64:arm64; do
    target="${arch%%:*}-unknown-linux-gnu"
    name="${arch##*:}"

    cargo zigbuild --release -p devshare-cli --target "$target.$GLIBC"
    cargo deb -p devshare-cli --no-build --no-strip --target "$target" \
        --output "dist/devshare_${version}_${name}.deb"
    tar -C "target/$target/release" -czf "dist/devshare-${version}-linux-${name}.tar.gz" devshare
done
ls -l dist
