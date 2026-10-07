# Everything builds and runs in Docker: no toolchain on the machine, and the
# guest gets the root, the TUN device and the network of its own it needs.

# Less debug information and fewer jobs at once than cargo's defaults: with
# full debug information the linker is killed for lack of memory when the
# Docker VM is shared with other running projects.
RUST = docker run --rm \
	-v "$(CURDIR)":/work \
	-v devshare-cargo:/usr/local/cargo/registry \
	-v devshare-target:/work/target \
	-e DEVSHARE_RELAY=disabled \
	-e CARGO_PROFILE_DEV_DEBUG=line-tables-only \
	-e CARGO_PROFILE_TEST_DEBUG=line-tables-only \
	-e CARGO_BUILD_JOBS=4 \
	-w /work rust:1-bookworm
CARGO = $(RUST) cargo

.PHONY: build release test tests check e2e deploy deb dist-macos relay macos ios app dmg showcase demo install uninstall clean

build:
	$(CARGO) build

release:
	$(CARGO) build --release

check:
	$(RUST) sh -c 'rustup component add clippy >/dev/null 2>&1 && cargo clippy --all-targets -- -D warnings'

# The automated tests. `make tests` is the same thing.
test tests:
	$(CARGO) test

# A host sharing its services with a guest that has only the code: once on
# one network, once on two networks joined by nothing but our own relay.
# Then the guest as an ordinary user with the privileged helper, and a guest
# that cannot reach the host nor any control plane, only a relay.
e2e: build relay
	docker/e2e.sh
	docker/e2e.sh relayed
	docker/e2e.sh helper
	docker/remote.sh
	deploy/test.sh

# The public server (relay and control plane behind Caddy) from deploy/, on
# the machine DEVSHARE_HOST points at. See deploy/compose.yml.
deploy:
	@test -n "$(DEVSHARE_HOST)" || { echo "set DEVSHARE_HOST, e.g. make deploy DEVSHARE_HOST=relay.join.glitchr.dev"; exit 1; }
	DEVSHARE_HOST=$(DEVSHARE_HOST) docker compose -f deploy/compose.yml up -d --build

# dist/: a .deb and a tarball for amd64 and arm64.
deb:
	docker build -q -t devshare-build -f docker/build.Dockerfile docker
	docker run --rm -v "$(CURDIR)":/work -v devshare-cargo:/usr/local/cargo/registry \
		-v devshare-target:/work/target -w /work devshare-build docker/package.sh

# dist/: the macOS commands, optimized, as a tarball for this machine's
# architecture. Not signed: macOS will ask before running them elsewhere.
dist-macos:
	PATH="$$(brew --prefix rustup)/bin:$$PATH" cargo build --release -p devshare-cli -p devshare-server --target-dir target-macos
	mkdir -p dist
	tar -C target-macos/release -czf \
		dist/devshare-$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)-macos-$$(uname -m).tar.gz \
		devshare devshare-server

# iroh's relay server, built once into the build volume.
relay:
	$(RUST) sh -c 'test -x target/relay/bin/iroh-relay || cargo install iroh-relay@1.3.0 --features server --root target/relay'

# The native binaries, in target-macos/debug. Needs rustup (brew install rustup).
macos:
	PATH="$$(brew --prefix rustup)/bin:$$PATH" cargo build --workspace --target-dir target-macos

# bin/: the commands, as links to the native build, so that a rebuild
# (make macos) is picked up at once. `source env.sh` puts bin/ on the PATH.
install: macos
	mkdir -p bin
	for command in devshare devshare-server devshare-helper devshare-app; do \
		ln -sf "../target-macos/debug/$$command" "bin/$$command"; \
	done
	@echo "The commands are in bin/. Put them on your PATH with: source $(CURDIR)/env.sh"

uninstall:
	rm -rf bin

# tests/showcase for real: its services in Docker, its devshare.toml read
# from its compose file, shared from this machine to a guest in a container.
showcase: build macos
	tests/showcase/run.sh

# The desktop app, run from the build folder.
app:
	PATH="$$(brew --prefix rustup)/bin:$$PATH" cargo run -p devshare-app --target-dir target-macos

# dist/DevShare.app and dist/DevShare-<version>-<arch>.dmg, unsigned. The
# disk image is made with hdiutil alone: Tauri's own bundler drives the
# Finder to lay it out, which opens windows on the desktop.
dmg:
	cd app && env -u SDKROOT PATH="$$(brew --prefix rustup)/bin:$$PATH" CARGO_TARGET_DIR=../target-macos \
		npx --yes @tauri-apps/cli@2 build
	rm -rf dist/DevShare.app dist/dmg && mkdir -p dist/dmg
	cp -R target-macos/release/bundle/macos/DevShare.app dist/DevShare.app
	cp -R dist/DevShare.app dist/dmg/ && ln -s /Applications dist/dmg/Applications
	hdiutil create -quiet -ov -fs HFS+ -volname DevShare -srcfolder dist/dmg \
		dist/DevShare-$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)-$$(uname -m).dmg
	rm -rf dist/dmg

# Checks that the core still builds for iOS. The shell's SDKROOT, when it
# points at the macOS SDK, breaks the link: it is cleared here.
ios:
	rustup target add aarch64-apple-ios >/dev/null 2>&1 || true
	env -u SDKROOT IPHONEOS_DEPLOYMENT_TARGET=16.0 PATH="$$(brew --prefix rustup)/bin:$$PATH" \
		cargo build -p devshare-core --release --target aarch64-apple-ios --target-dir target-macos

# The showcase started and shared in one go: its services, the invitation and
# its QR code in this terminal. Ctrl-C stops both.
demo:
	@CARGO_TERM_QUIET=true $(MAKE) --no-print-directory -s install >/dev/null
	@tests/showcase/demo.sh

clean:
	docker volume rm -f devshare-target
