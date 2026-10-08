#!/bin/sh
# The iPhone app: the Rust core as a static library, its Swift bindings, and
# the Xcode project, all generated. Then open ios/DevShare.xcodeproj and run
# it on an iPhone (iOS 17 or later).
#
#   ios/build.sh                the library, the bindings and the project
#   ios/build.sh --check        the same, then compiles the app, unsigned
#
# The signing team is the one of this Mac's Apple Development certificate;
# DEVSHARE_TEAM=<team id> chooses another.
set -eu
cd "$(dirname "$0")/.."
export PATH="$(brew --prefix rustup)/bin:$PATH" CARGO_INCREMENTAL=0 IPHONEOS_DEPLOYMENT_TARGET=17.0
# This shell's SDK is the Mac's: the iPhone's comes from the target.
unset SDKROOT
target=target-macos

rustup target add aarch64-apple-ios >/dev/null 2>&1 || true
cargo build -q -p devshare-mobile --release --target aarch64-apple-ios --target-dir "$target"
# The bindings are read from a library built for this Mac.
cargo build -q -p devshare-mobile --target-dir "$target"

rm -rf ios/Generated ios/Core.xcframework
mkdir -p ios/Generated/headers
cargo run -q -p devshare-bindgen --target-dir "$target" -- generate \
    --library "$target/debug/libdevshare_mobile.dylib" --language swift --out-dir ios/Generated
mv ios/Generated/devshare_mobileFFI.h ios/Generated/headers/
mv ios/Generated/devshare_mobileFFI.modulemap ios/Generated/headers/module.modulemap
xcodebuild -create-xcframework \
    -library "$target/aarch64-apple-ios/release/libdevshare_mobile.a" -headers ios/Generated/headers \
    -output ios/Core.xcframework

# The teams of this Mac's certificates named like $1, one per line.
teams() {
    security find-certificate -a -c "$1" -p 2>/dev/null | awk '
        /BEGIN CERTIFICATE/ { pem = "" }
        { pem = pem $0 "\n" }
        /END CERTIFICATE/ { printf "%s", pem | "openssl x509 -noout -subject"; close("openssl x509 -noout -subject") }' \
        | sed -n 's/.*OU *= *\([A-Z0-9]*\).*/\1/p' | sort -u
}
# A team that can also sign for distribution (a paid membership) wins over a
# personal one.
development=$(teams "Apple Development")
paid=$(teams "Developer ID Application" | while read -r candidate; do
    echo "$development" | grep -qx "$candidate" && echo "$candidate"
done | head -1)
team=${DEVSHARE_TEAM:-${paid:-$(echo "$development" | head -1)}}
(cd ios && DEVSHARE_TEAM="$team" xcodegen generate --quiet)
echo "ios/DevShare.xcodeproj is ready${team:+, signed by team $team}: open it, pick your iPhone, run."

if [ "${1:-}" = --check ]; then
    xcodebuild -quiet -project ios/DevShare.xcodeproj -scheme DevShare -configuration Debug \
        -destination 'generic/platform=iOS' -derivedDataPath "$target/ios-derived" \
        CODE_SIGNING_ALLOWED=NO build
    echo "The app compiles."
fi
