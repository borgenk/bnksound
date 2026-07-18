.PHONY: fmt clippy build build-release build-linux install install-gtk \
	install-assets run test check bump \
	build-native build-native-release run-native \
	build-gtk build-gtk-release run-gtk test-matrix

APP_NAME := bnksound
APP_ID := io.github.borgenk.BnkSound
BUILD_PATH := target/release
VERSION := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
LINUX_TARGET := x86_64-unknown-linux-gnu
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

# Build a release tarball into dist/ for upload to a GitHub Release.
# Bundles the binary, desktop entry, and icon tree so install.sh can place them all.
#
# What ships is the GTK build, staged under the plain name bnksound: a download
# gets the GTK shell and needs GTK 4 at runtime. Building from source is the
# other way round, `cargo build` with no features gives the native binary under
# that same name.
build-linux:
	RUSTFLAGS="--remap-path-prefix=$(HOME)=[home]" \
		cargo build --release --features gtk --bin $(APP_NAME)-gtk --target $(LINUX_TARGET)
	rm -rf dist/stage
	mkdir -p dist/stage/icons
	cp target/$(LINUX_TARGET)/release/$(APP_NAME)-gtk dist/stage/$(APP_NAME)
	cp assets/$(APP_ID).desktop dist/stage/$(APP_ID).desktop
	cp -r assets/icons/hicolor dist/stage/icons/hicolor
	tar czf dist/$(APP_NAME)-v$(VERSION)-$(LINUX_TARGET).tar.gz -C dist/stage .
	rm -rf dist/stage
	@echo "Built dist/$(APP_NAME)-v$(VERSION)-$(LINUX_TARGET).tar.gz"
	@echo "Publish with: gh release create v$(VERSION) dist/$(APP_NAME)-v$(VERSION)-$(LINUX_TARGET).tar.gz"

run:
	cargo run

test:
	cargo test

check: fmt clippy test-matrix
