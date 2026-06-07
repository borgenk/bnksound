#!/usr/bin/env sh
set -eu

# Downloads a release from GitHub and installs it into ~/.local/bin, along with
# the desktop entry and icon theme so it shows up in your app menu.
# Pin a specific version with BNKSOUND_VERSION=v0.1.0.

REPO="borgenk/bnksound"
APP_ID="io.github.borgenk.BnkSound"

main() {
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

    echo "Downloading bnksound ${VERSION} for ${TARGET}..."
    fetch "$URL" "$TMP_DIR/$FILENAME"

    echo "Extracting..."
    tar -xzf "$TMP_DIR/$FILENAME" -C "$TMP_DIR"

    INSTALL_DIR="${HOME}/.local/bin"
    mkdir -p "$INSTALL_DIR"
    mv "$TMP_DIR/bnksound" "$INSTALL_DIR/bnksound"
    chmod +x "$INSTALL_DIR/bnksound"

    echo "Installed bnksound ${VERSION} to ${INSTALL_DIR}/bnksound"

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
