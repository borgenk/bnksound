.PHONY: fmt clippy build build-release build-linux install install-gtk \
	install-assets run test check bump \
	build-native build-native-release run-native \
	build-gtk build-gtk-release run-gtk test-matrix tables perf perf-save frame

# The toolchain is nightly (rust-toolchain.toml) and .cargo/config.toml builds
# the standard library from source alongside the app. Release binaries come out
# around a third smaller than a stable build and paint a few percent faster,
# because LTO reaches std and release panics abort where they happen instead of
# unwinding through a formatter. The cost is that a release panic prints
# nothing at all. Debug builds and the test harness are untouched: the panic
# strategy is set on the release profile only.
APP_NAME := bnksound
APP_ID := io.github.borgenk.BnkSound
VERSION := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
LINUX_TARGET := x86_64-unknown-linux-gnu
# Everything lands under the target triple, since .cargo/config.toml names one.
BUILD_PATH := target/$(LINUX_TARGET)/release
BIN_DIR := ~/.local/bin
APPS_DIR := ~/.local/share/applications
ICON_DIR := ~/.local/share/icons/hicolor

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all --benches --tests --examples --all-features -- -D warnings

build:
	cargo build

build-release:
	cargo build --release

# --- Native / GTK build matrix ----------------------------------------------
# Two binaries, both painting through the same software renderer:
#   bnksound      the default, GTK-free Wayland app
#   bnksound-gtk  opt-in, GTK owns the window (built with --features gtk)
# The plain build/test/run targets above are the native one.
build-native:
	cargo build --bin bnksound
build-native-release:
	cargo build --release --bin bnksound
run-native:
	cargo run --bin bnksound

# The GTK variant. Its binary is bnksound-gtk, gated behind the gtk feature.
build-gtk:
	cargo build --features gtk --bin bnksound-gtk
build-gtk-release:
	cargo build --release --features gtk --bin bnksound-gtk
run-gtk:
	cargo run --features gtk --bin bnksound-gtk

# Both feature sets, which is what CI gates on: the native build must stay
# GTK-free and the GTK build must keep working.
test-matrix:
	cargo test
	cargo test --features gtk

# --- Development tooling (src/dev/) ------------------------------------------
# All of it hangs off flags on the native binary, behind the `dev` feature, so
# the shipping build carries none of it.

# Regenerate the Unicode grapheme tables from the data vendored in ucd/. Pure,
# offline, and deterministic: on an unchanged ucd/ this rewrites the committed
# file with identical bytes.
tables:
	cargo run --features dev -- --gen-tables

# Time the hot paths and compare against perf/baseline.txt, failing on a
# regression. Release only: a debug build measures the wrong program. The
# allocator that counts allocations comes in with perf-alloc. Not run in CI,
# where a shared runner's timings say more about the runner than the code.
perf:
	cargo run --release --features perf-alloc -- --perf

# Accept the current numbers as the new baseline, to be committed with whatever
# change moved them.
perf-save:
	cargo run --release --features perf-alloc -- --perf --save

# Paint one frame to a PNG without a compositor, for looking at the UI.
# Pass a path, width, and height: make frame ARGS="out.png 800 900"
frame:
	cargo run --features dev -- --render-frame $(ARGS)

# Install the release binary, desktop entry, and icons under ~/.local, the same
# per-user location install.sh uses.
#
# Either variant installs as bnksound and the two overwrite each other, so the
# desktop entry and the command are the same whichever you picked. The -gtk
# suffix exists only in target/, where cargo needs two names.
install: build-release install-assets
	install -Dm755 $(BUILD_PATH)/$(APP_NAME) $(BIN_DIR)/$(APP_NAME)
	@echo "Installed the native $(APP_NAME) to ~/.local/bin/"

install-gtk: build-gtk-release install-assets
	install -Dm755 $(BUILD_PATH)/$(APP_NAME)-gtk $(BIN_DIR)/$(APP_NAME)
	@echo "Installed the GTK $(APP_NAME) to ~/.local/bin/"

# Desktop entry and icon theme, the same for both variants since the entry runs
# bnksound either way.
# The icon cache / desktop database refreshes are best-effort (ignored if the tools are absent).
install-assets:
	install -Dm644 assets/$(APP_ID).desktop $(APPS_DIR)/$(APP_ID).desktop
	mkdir -p $(ICON_DIR)
	cp -r assets/icons/hicolor/. $(ICON_DIR)/
	-gtk-update-icon-cache -f -t $(ICON_DIR)
	-update-desktop-database $(APPS_DIR)
	@echo "Installed desktop file + icons to ~/.local/share/"

# Bump version, commit, and tag: make bump V=0.2.0
# Pushing the tag triggers the release workflow, which rejects any tag whose
# name does not match this version, so the two stay in lockstep.
bump:
	@test -n "$(V)" || (echo "Current: $(VERSION). Usage: make bump V=0.2.0" && exit 1)
	sed -i '0,/^version = ".*"/{s//version = "$(V)"/}' Cargo.toml
	cargo update --workspace
	git add Cargo.toml Cargo.lock
	git commit -m "Bump version to $(V)"
	git tag "v$(V)"
	@echo "Bumped to v$(V). Push with: git push origin main --tags"

# Build the release tarballs into dist/ for upload to a GitHub Release.
#
# One archive per variant, so a download carries the build it is for and not
# both. Each holds its binary under the plain name bnksound, alongside the same
# desktop entry and icon tree, which is why the staging tree is built once and
# only the binary swapped. The GTK archive keeps the unsuffixed name it has
# always had, so an installer from before the split still resolves it.
#
# Each archive ships with a sha256 beside it, which install.sh checks before it
# unpacks anything. The digest names the bare file, so `sha256sum -c` works by
# hand in dist/ too.
TARBALL_GTK := $(APP_NAME)-v$(VERSION)-$(LINUX_TARGET).tar.gz
TARBALL_UND := $(APP_NAME)-undecorated-v$(VERSION)-$(LINUX_TARGET).tar.gz

build-linux:
	RUSTFLAGS="--remap-path-prefix=$(HOME)=[home]" \
		cargo build --release --features gtk --bin $(APP_NAME)-gtk
	RUSTFLAGS="--remap-path-prefix=$(HOME)=[home]" \
		cargo build --release --bin $(APP_NAME)
	rm -rf dist/stage
	mkdir -p dist/stage/icons
	cp assets/$(APP_ID).desktop dist/stage/$(APP_ID).desktop
	cp -r assets/icons/hicolor dist/stage/icons/hicolor
	cp $(BUILD_PATH)/$(APP_NAME)-gtk dist/stage/$(APP_NAME)
	tar czf dist/$(TARBALL_GTK) -C dist/stage .
	cp $(BUILD_PATH)/$(APP_NAME) dist/stage/$(APP_NAME)
	tar czf dist/$(TARBALL_UND) -C dist/stage .
	rm -rf dist/stage
	cd dist && sha256sum $(TARBALL_GTK) > $(TARBALL_GTK).sha256
	cd dist && sha256sum $(TARBALL_UND) > $(TARBALL_UND).sha256
	@echo "Built dist/$(TARBALL_GTK) and dist/$(TARBALL_UND), each with a .sha256"
	@echo "Publish with: gh release create v$(VERSION) dist/$(TARBALL_GTK)* dist/$(TARBALL_UND)*"

run:
	cargo run

test:
	cargo test

check: fmt clippy test-matrix
