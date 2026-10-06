#!/usr/bin/env sh
set -eu

# Downloads a release from GitHub and installs it into ~/.local/bin, along with
# the desktop entry and icon theme so it shows up in your app menu.
# Pin a specific version with BNKSOUND_VERSION=v0.1.0.
#
# Two builds ship, one archive each, and both install as bnksound:
#
#   sh install.sh                  the GTK build, needs GTK 4 at runtime
#   sh install.sh --undecorated    no window decorations, no GTK
#
# Through a pipe the flag goes after -s --:
#   curl -fsSL <url> | sh -s -- --undecorated

REPO="borgenk/bnksound"
APP_ID="io.github.borgenk.BnkSound"

# A path as an Exec field. Percent is the field-code escape, so it doubles, and
# a path holding anything the spec reserves has to be quoted.
desktop_exec() {
    path="$(printf '%s' "$1" | sed 's/%/%%/g')"
    case "$path" in
        *[!A-Za-z0-9/._+-]*)
            printf '"%s"' "$(printf '%s' "$path" | sed 's/[\\"`$]/\\&/g')"
            ;;
        *)
            printf '%s' "$path"
            ;;
    esac
}

# Copy the desktop entry with Exec naming the binary by absolute path. A
# launcher resolves a bare name against the session's PATH, which need not
# carry ~/.local/bin.
write_desktop_entry() {
    src="$1"
    dest="$2"
    field="$(desktop_exec "$3")"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            Exec=*) printf '%s\n' "Exec=$field" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$src" > "$dest"
}

# Pick the downloader. Both handle file:// URLs, which is what lets the
# installer test drive this path with no network.
select_fetch() {
    if command -v curl > /dev/null 2>&1; then
        fetch() { curl -fsSL "$1" -o "$2"; }
        fetch_stdout() { curl -fsSL "$1"; }
    elif command -v wget > /dev/null 2>&1; then
        fetch() { wget -q "$1" -O "$2"; }
        fetch_stdout() { wget -q "$1" -O -; }
    else
        echo "Error: curl or wget is required"
        return 1
    fi
}

# The download's sha256, or non-zero when the machine has nothing to compute it
# with.
sha256_of() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v openssl > /dev/null 2>&1; then
        openssl dgst -sha256 "$1" | awk '{print $NF}'
    else
        return 1
    fi
}

# Check a download against the sha256 published beside it, and refuse when that
# cannot be done: a checksum that will not download and a machine with no digest
# tool look the same as a tampered release.
verify_checksum() {
    file="$1"
    sums_url="$2"

    if [ "${BNKSOUND_SKIP_CHECKSUM:-}" = "1" ]; then
        echo "Warning: BNKSOUND_SKIP_CHECKSUM=1, installing a download nothing has checked"
        return 0
    fi

    if ! actual="$(sha256_of "$file")"; then
        echo "Error: verifying the download needs sha256sum, shasum or openssl"
        echo "  install one, or set BNKSOUND_SKIP_CHECKSUM=1 to install unverified"
        return 1
    fi

    if ! fetch "$sums_url" "$file.sha256" 2> /dev/null; then
        echo "Error: cannot download the checksum for this release"
        echo "  $sums_url"
        echo "  set BNKSOUND_SKIP_CHECKSUM=1 to install unverified"
        return 1
    fi

    # The published file is sha256sum output, so the digest is its first field.
    expected="$(cut -d' ' -f1 < "$file.sha256")"
    if [ "${#expected}" -ne 64 ] || [ -n "$(printf '%s' "$expected" | tr -d '0-9a-fA-F')" ]; then
        echo "Error: the published checksum is not a sha256 digest, refusing to install"
        return 1
    fi
    expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
    actual="$(printf '%s' "$actual" | tr 'A-F' 'a-f')"

    if [ "$expected" != "$actual" ]; then
        echo "Error: checksum mismatch, refusing to install"
        echo "  expected: $expected"
        echo "  actual:   $actual"
        return 1
    fi

    echo "Checksum verified"
}

# Which archive to fetch. Each holds its binary under the plain name bnksound,
# so only the name of the download changes with the flag.
ARCHIVE="bnksound"
VARIANT_NAME="GTK"

main() {
    for arg in "$@"; do
        case "$arg" in
            --undecorated)
                ARCHIVE="bnksound-undecorated"
                VARIANT_NAME="undecorated" ;;
            --gtk)
                ARCHIVE="bnksound"
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

    select_fetch || exit 1

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

    FILENAME="${ARCHIVE}-${VERSION}-${TARGET}.tar.gz"
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${FILENAME}"

    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        TMP_DIR="$(mktemp -d "$TMPDIR/bnksound-XXXXXX")"
    else
        TMP_DIR="$(mktemp -d "/tmp/bnksound-XXXXXX")"
    fi
    trap 'rm -rf "$TMP_DIR"' EXIT

    echo "Downloading bnksound ${VERSION} for ${TARGET} (${VARIANT_NAME} build)..."
    if ! fetch "$URL" "$TMP_DIR/$FILENAME"; then
        echo "Error: could not download the ${VARIANT_NAME} build of ${VERSION}"
        echo "  $URL"
        if [ "$ARCHIVE" != "bnksound" ]; then
            echo "Releases before v0.3.3 shipped one archive holding both builds."
        fi
        exit 1
    fi

    verify_checksum "$TMP_DIR/$FILENAME" "${URL}.sha256" || exit 1

    echo "Extracting..."
    tar -xzf "$TMP_DIR/$FILENAME" -C "$TMP_DIR"

    if [ ! -f "$TMP_DIR/bnksound" ]; then
        echo "Error: the archive holds no bnksound binary"
        exit 1
    fi

    INSTALL_DIR="${HOME}/.local/bin"
    mkdir -p "$INSTALL_DIR"
    mv "$TMP_DIR/bnksound" "$INSTALL_DIR/bnksound"
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
            if [ "$ARCHIVE" = "bnksound" ]; then
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
        write_desktop_entry "$TMP_DIR/${APP_ID}.desktop" \
            "$APPS_DIR/${APP_ID}.desktop" "$INSTALL_DIR/bnksound"
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

# Sourced as a library by the Makefile and the installer test, run otherwise.
if [ "${BNKSOUND_INSTALL_LIB:-}" != "1" ]; then
    main "$@"
fi
