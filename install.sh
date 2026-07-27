#!/usr/bin/env sh
set -eu

# Downloads a release from GitHub and installs it into ~/.local/bin, along with
# the desktop entry and icon theme so it shows up in your app menu.
# Pin a specific version with BNKSOUND_VERSION=v0.1.0.
#
# Two builds ship in the same tarball and both install as bnksound:
#
#   sh install.sh                  the GTK build, needs GTK 4 at runtime
#   sh install.sh --undecorated    no window decorations, no GTK
#
# Through a pipe the flag goes after -s --:
#   curl -fsSL <url> | sh -s -- --undecorated

REPO="borgenk/bnksound"
APP_ID="io.github.borgenk.BnkSound"

# Check a download against the sha256 published beside it. A mismatch stops the
# install; anything else that goes wrong only warns, since a release from before
# checksums existed, or a machine with no digest tool, should still install.
verify_checksum() {
    file="$1"
    sums_url="$2"

    if ! fetch "$sums_url" "$file.sha256" 2> /dev/null; then
        echo "Note: this release publishes no checksum, skipping verification"
        return 0
    fi

    if command -v sha256sum > /dev/null 2>&1; then
        actual="$(sha256sum "$file" | cut -d' ' -f1)"
    elif command -v shasum > /dev/null 2>&1; then
        actual="$(shasum -a 256 "$file" | cut -d' ' -f1)"
    elif command -v openssl > /dev/null 2>&1; then
        actual="$(openssl dgst -sha256 "$file" | awk '{print $NF}')"
    else
        echo "Note: no sha256 tool found, skipping verification"
        return 0
    fi

    # The published file is `sha256sum` output, so the digest is its first field.
    expected="$(cut -d' ' -f1 < "$file.sha256")"

    if [ "$expected" != "$actual" ]; then
        echo "Error: checksum mismatch, refusing to install"
        echo "  expected: $expected"
        echo "  actual:   $actual"
        exit 1
    fi

    echo "Checksum verified"
}

# Which file inside the tarball to install. The flag swaps it for the other one.
VARIANT="bnksound"
VARIANT_NAME="GTK"

main() {
    for arg in "$@"; do
        case "$arg" in
            --undecorated)
                VARIANT="bnksound-undecorated"
                VARIANT_NAME="undecorated" ;;
            --gtk)
                VARIANT="bnksound"
                VARIANT_NAME="GTK" ;;
            -h | --help)
                echo "Usage: install.sh [--gtk | --undecorated]"
                echo "  --gtk          the GTK build (default), needs GTK 4"
                echo "  --undecorated  no window decorations, no GTK"
                exit 0 ;;
            *)
                echo "Error: unknown option: $arg (try --help)"
                exit 1 ;;
        esac
    done

    platform="$(uname -s)"
    arch="$(uname -m)"

    case "$platform" in
        Linux)
            case "$arch" in
                x86_64 | x86-64 | x64 | amd64)
                    TARGET="x86_64-unknown-linux-gnu" ;;
                *)
                    echo "Error: unsupported architecture: $arch"
                    exit 1 ;;
            esac
            ;;
        *)
            echo "Error: unsupported OS: $platform (bnksound is Linux-only)"
            exit 1
            ;;
    esac

    if command -v curl > /dev/null 2>&1; then
        fetch() { curl -fsSL "$1" -o "$2"; }
        fetch_stdout() { curl -fsSL "$1"; }
    elif command -v wget > /dev/null 2>&1; then
        fetch() { wget -q "$1" -O "$2"; }
        fetch_stdout() { wget -q "$1" -O -; }
    else
        echo "Error: curl or wget is required"
        exit 1
    fi

    if [ -n "${BNKSOUND_VERSION:-}" ]; then
        VERSION="$BNKSOUND_VERSION"
    else
        echo "Resolving latest version..."
        VERSION="$(fetch_stdout "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' \
            | head -n1 \
            | cut -d'"' -f4)"
        if [ -z "$VERSION" ]; then
            echo "Error: failed to resolve latest version"
            exit 1
        fi
    fi

    FILENAME="bnksound-${VERSION}-${TARGET}.tar.gz"
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${FILENAME}"

    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        TMP_DIR="$(mktemp -d "$TMPDIR/bnksound-XXXXXX")"
    else
        TMP_DIR="$(mktemp -d "/tmp/bnksound-XXXXXX")"
    fi
    trap 'rm -rf "$TMP_DIR"' EXIT

    echo "Downloading bnksound ${VERSION} for ${TARGET} (${VARIANT_NAME} build)..."
    fetch "$URL" "$TMP_DIR/$FILENAME"

    verify_checksum "$TMP_DIR/$FILENAME" "${URL}.sha256"

    echo "Extracting..."
    tar -xzf "$TMP_DIR/$FILENAME" -C "$TMP_DIR"

    if [ ! -f "$TMP_DIR/$VARIANT" ]; then
        echo "Error: this release has no ${VARIANT_NAME} build (${VARIANT} is not"
        echo "in the tarball). Older releases shipped the GTK build only."
        exit 1
    fi

    INSTALL_DIR="${HOME}/.local/bin"
    mkdir -p "$INSTALL_DIR"
    mv "$TMP_DIR/$VARIANT" "$INSTALL_DIR/bnksound"
    chmod +x "$INSTALL_DIR/bnksound"

    echo "Installed bnksound ${VERSION} (${VARIANT_NAME} build) to ${INSTALL_DIR}/bnksound"

    # Name what the loader cannot find. Launched from the app menu a missing
    # library is a window that never appears, so say it here instead.
    if command -v ldd > /dev/null 2>&1; then
        missing="$(ldd "$INSTALL_DIR/bnksound" 2>/dev/null | grep 'not found' || true)"
        if [ -n "$missing" ]; then
            echo ""
            echo "Warning: bnksound will not start, these libraries are missing:"
            echo "$missing" | awk '{print "  " $1}'
            if [ "$VARIANT" = "bnksound" ]; then
                echo ""
                echo "GTK 4 is usually the one: install libgtk-4-1 (Debian, Ubuntu)"
                echo "or gtk4 (Arch, Fedora), or reinstall with --undecorated."
            fi
        fi
    fi

    # Desktop entry + icon theme, bundled in the tarball. Best-effort: a missing
    # piece or absent cache tool never fails the binary install.
    DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
    APPS_DIR="$DATA_HOME/applications"
    ICONS_DIR="$DATA_HOME/icons/hicolor"
    if [ -f "$TMP_DIR/${APP_ID}.desktop" ]; then
        mkdir -p "$APPS_DIR"
        cp "$TMP_DIR/${APP_ID}.desktop" "$APPS_DIR/${APP_ID}.desktop"
        update-desktop-database "$APPS_DIR" > /dev/null 2>&1 || true
    fi
    if [ -d "$TMP_DIR/icons/hicolor" ]; then
        mkdir -p "$ICONS_DIR"
        cp -r "$TMP_DIR/icons/hicolor/." "$ICONS_DIR/"
        gtk-update-icon-cache -f -t "$ICONS_DIR" > /dev/null 2>&1 || true
    fi

    if [ "$(command -v bnksound)" = "$INSTALL_DIR/bnksound" ]; then
        echo "Run with 'bnksound'"
    else
        echo ""
        echo "To run bnksound from your terminal, add ~/.local/bin to your PATH:"

        case "${SHELL:-}" in
            *zsh)
                echo "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "  source ~/.zshrc"
                ;;
            *fish)
                echo "  fish_add_path -U \$HOME/.local/bin"
                ;;
            *)
                echo "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "  source ~/.bashrc"
                ;;
        esac
    fi
}

main "$@"
