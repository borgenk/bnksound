.PHONY: fmt fmt-check clippy build build-release build-linux install install-gtk \
	install-assets run test check bump \
	build-native build-native-release run-native \
	build-gtk build-gtk-release run-gtk test-matrix tables perf perf-save frame \
	screenshot test-install test-abi \
	test-compositor

APP_NAME := bnksound
APP_ID := io.github.borgenk.BnkSound
VERSION := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
LINUX_TARGET := x86_64-unknown-linux-gnu
GLIBC_FLOOR := 2.34
BUILD_PATH := target/$(LINUX_TARGET)/release
# Absolute, since the installed desktop entry names the binary by full path.
BIN_DIR := $(HOME)/.local/bin
APPS_DIR := $(HOME)/.local/share/applications
ICON_DIR := $(HOME)/.local/share/icons/hicolor

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all --benches --tests --examples --all-features -- -D warnings

build:
	cargo build

build-release:
	cargo build --release

# --- Native / GTK build matrix ----------------------------------------------
#   bnksound      the default, GTK-free Wayland app
#   bnksound-gtk  opt-in, GTK owns the window
build-native:
	cargo build --bin bnksound
build-native-release:
	cargo build --release --bin bnksound
run-native:
	cargo run --bin bnksound

build-gtk:
	cargo build --features gtk --bin bnksound-gtk
build-gtk-release:
	cargo build --release --features gtk --bin bnksound-gtk
run-gtk:
	cargo run --features gtk --bin bnksound-gtk

# Both feature sets, which is what CI gates on.
test-matrix:
	cargo test
	cargo test --features gtk

# --- Development tooling (src/dev/) ------------------------------------------
# All of it hangs off flags on the native binary, behind the dev feature, so
# the shipping build carries none of it.

# Regenerate the Unicode grapheme tables from the data vendored in ucd/.
# Deterministic: an unchanged ucd/ rewrites the committed file byte for byte.
tables:
	cargo run --features dev -- --gen-tables

# Time the hot paths against perf/baseline.txt, failing on a regression.
# Release only: a debug build measures the wrong program. Not run in CI, where a
# shared runner's timings say more about the runner than the code.
perf:
	cargo run --release --features perf-alloc -- --perf

# Accept the current numbers as the new baseline.
perf-save:
	cargo run --release --features perf-alloc -- --perf --save

# Paint one frame to a PNG without a compositor, for looking at the UI.
# Pass a path, width, and height: make frame ARGS="out.png 800 900"
frame:
	cargo run --features dev -- --render-frame $(ARGS)

screenshot:
	cargo run --features dev -- --render-frame assets/screenshot.png 518 292

# The installer's desktop entry and checksums, with no network.
test-install:
	sh .github/scripts/test-install.sh

# The ABI floor check, against binaries this machine already has.
test-abi:
	sh .github/scripts/test-abi.sh

# The Wayland protocol code against real compositors: headless weston, labwc and
# cage. Ignored by default, and serial, since every test boots a compositor.
test-compositor:
	cargo test --features dev --test compositor -- --ignored --test-threads=1

# Install the release binary, desktop entry, and icons under ~/.local, the same
# per-user location install.sh uses. Either variant installs as bnksound.
install: build-release install-assets
	install -Dm755 $(BUILD_PATH)/$(APP_NAME) $(BIN_DIR)/$(APP_NAME)
	@echo "Installed the native $(APP_NAME) to ~/.local/bin/"

install-gtk: build-gtk-release install-assets
	install -Dm755 $(BUILD_PATH)/$(APP_NAME)-gtk $(BIN_DIR)/$(APP_NAME)
	@echo "Installed the GTK $(APP_NAME) to ~/.local/bin/"

# Desktop entry and icon theme, the same for both variants since both install as
# bnksound. install.sh writes the entry, so its Exec rule lives in one place.
install-assets:
	@mkdir -p $(APPS_DIR)
	BNKSOUND_INSTALL_LIB=1 sh -c '. ./install.sh; write_desktop_entry "$$1" "$$2" "$$3"' \
		sh assets/$(APP_ID).desktop $(APPS_DIR)/$(APP_ID).desktop $(BIN_DIR)/$(APP_NAME)
	mkdir -p $(ICON_DIR)
	cp -r assets/icons/hicolor/. $(ICON_DIR)/
	-gtk-update-icon-cache -f -t $(ICON_DIR)
	-update-desktop-database $(APPS_DIR)
	@echo "Installed desktop file + icons to ~/.local/share/"

# Bump version, commit, and tag: make bump V=0.2.0
# The release workflow rejects a tag that does not match this version. The
# metainfo entry is what the Flatpak reports as its release history.
bump:
	@test -n "$(V)" || (echo "Current: $(VERSION). Usage: make bump V=0.2.0" && exit 1)
	sed -i '0,/^version = ".*"/{s//version = "$(V)"/}' Cargo.toml
	sed -i 's|<releases>|<releases>\n    <release version="$(V)" date="'"$$(date -u +%F)"'"/>|' assets/$(APP_ID).metainfo.xml
	cargo update --workspace
	git add Cargo.toml Cargo.lock assets/$(APP_ID).metainfo.xml
	git commit -m "Bump version to $(V)"
	git tag "v$(V)"
	@echo "Bumped to v$(V). Push with: git push origin main --tags"

# Build the release tarballs into dist/ for upload to a GitHub Release. One
# archive per variant, each holding its binary under the plain name bnksound, so
# the staging tree is built once and only the binary swapped. The GTK archive
# keeps the unsuffixed name, which older installers resolve.
TARBALL_GTK := $(APP_NAME)-v$(VERSION)-$(LINUX_TARGET).tar.gz
TARBALL_UND := $(APP_NAME)-undecorated-v$(VERSION)-$(LINUX_TARGET).tar.gz

build-linux:
	RUSTFLAGS="--remap-path-prefix=$(HOME)=[home]" \
		cargo build --release --features gtk --bin $(APP_NAME)-gtk
	RUSTFLAGS="--remap-path-prefix=$(HOME)=[home]" \
		cargo build --release --bin $(APP_NAME)
	sh .github/scripts/check-abi.sh $(GLIBC_FLOOR) \
		$(BUILD_PATH)/$(APP_NAME) $(BUILD_PATH)/$(APP_NAME)-gtk
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

# The gate. Checks and reports, never rewrites: fmt is the target that edits.
check: fmt-check clippy test-matrix test-install test-abi
