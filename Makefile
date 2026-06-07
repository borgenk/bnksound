.PHONY: fmt clippy build build-release build-linux install run test check

APP_NAME := bnksound
APP_ID := io.github.borgenk.BnkSound
BUILD_PATH := target/release
VERSION := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
LINUX_TARGET := x86_64-unknown-linux-gnu
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

# Install release binary to /usr/local/bin and the desktop entry + icons to ~/.local/share.
# The icon cache / desktop database refreshes are best-effort (ignored if the tools are absent).
install: build-release
	sudo install -Dm755 $(BUILD_PATH)/$(APP_NAME) /usr/local/bin/$(APP_NAME)
	install -Dm644 assets/$(APP_ID).desktop $(APPS_DIR)/$(APP_ID).desktop
	mkdir -p $(ICON_DIR)
	cp -r assets/icons/hicolor/. $(ICON_DIR)/
	-gtk-update-icon-cache -f -t $(ICON_DIR)
	-update-desktop-database $(APPS_DIR)
	@echo "Installed $(APP_NAME) to /usr/local/bin/"
	@echo "Installed desktop file + icons to ~/.local/share/"

# Build a release tarball into dist/ for upload to a GitHub Release.
# Bundles the binary, desktop entry, and icon tree so install.sh can place them all.
build-linux:
	RUSTFLAGS="--remap-path-prefix=$(HOME)=[home]" \
		cargo build --release --target $(LINUX_TARGET)
	rm -rf dist/stage
	mkdir -p dist/stage/icons
	cp target/$(LINUX_TARGET)/release/$(APP_NAME) dist/stage/$(APP_NAME)
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

check: fmt clippy test
