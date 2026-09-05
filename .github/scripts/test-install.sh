#!/usr/bin/env sh
set -eu

# The installer, checked without a network or a desktop session: the Exec field
# an install writes, the escaping the desktop spec asks for, and the checksum
# verification, driven over file:// URLs.

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
APP_ID="io.github.borgenk.BnkSound"
TRACKED="assets/$APP_ID.desktop"

BNKSOUND_INSTALL_LIB=1 . ./install.sh

failures=0
check() {
    if [ "$2" = "$3" ]; then
        echo "ok   $1"
    else
        echo "FAIL $1"
        echo "     want: $2"
        echo "     got:  $3"
        failures=$((failures + 1))
    fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# A path a launcher can run as it stands goes in unquoted.
check "plain path" \
    "/home/u/.local/bin/bnksound" \
    "$(desktop_exec /home/u/.local/bin/bnksound)"

# Reserved characters mean the whole argument is quoted.
check "path with a space" \
    '"/home/a b/.local/bin/bnksound"' \
    "$(desktop_exec "/home/a b/.local/bin/bnksound")"

# Percent is the field-code escape, so a literal one doubles.
check "path with a percent" \
    '"/home/o%%d/bnksound"' \
    "$(desktop_exec "/home/o%d/bnksound")"

# Inside quotes the spec names the characters a backslash has to precede.
check "path with a quote" \
    '"/home/q\"t/bnksound"' \
    "$(desktop_exec '/home/q"t/bnksound')"

HOME="$TMP/home" make -s install-assets > /dev/null 2>&1
INSTALLED="$TMP/home/.local/share/applications/$APP_ID.desktop"

check "entry installed" "yes" "$([ -f "$INSTALLED" ] && echo yes || echo no)"
check "exec names the installed binary" \
    "Exec=$TMP/home/.local/bin/bnksound" \
    "$(grep '^Exec=' "$INSTALLED")"
check "nothing else changed" \
    "$(grep -v '^Exec=' "$TRACKED")" \
    "$(grep -v '^Exec=' "$INSTALLED")"

# The tracked entry keeps the bare name: Flatpak and distro packages run from a
# directory that is always on PATH, where an absolute home path would be wrong.
check "tracked entry untouched" "Exec=bnksound" "$(grep '^Exec=' "$TRACKED")"

# A launcher never sees the shell's PATH, so run what the entry actually says
# with .local/bin taken off PATH and check it reaches the installed binary.
mkdir -p "$TMP/home/.local/bin"
printf '#!/bin/sh\necho launched\n' > "$TMP/home/.local/bin/bnksound"
chmod +x "$TMP/home/.local/bin/bnksound"
without_local_bin="$(printf '%s' "$PATH" | tr ':' '\n' | grep -v '\.local/bin' | paste -sd: -)"
exec_field="$(grep '^Exec=' "$INSTALLED" | cut -d= -f2-)"
check "exec runs with .local/bin off PATH" \
    "launched" \
    "$(PATH="$without_local_bin" sh -c "$exec_field" 2>&1 || echo "did not run")"

# Checksum verification, driven over file:// URLs so the real downloader runs
# with no network.
select_fetch

mkdir -p "$TMP/remote"
printf 'archive contents\n' > "$TMP/remote/pkg.tar.gz"
good="$(sha256_of "$TMP/remote/pkg.tar.gz")"

# One archive per case, so a fetched digest never lands on another's.
staged() {
    mkdir -p "$TMP/$1"
    cp "$TMP/remote/pkg.tar.gz" "$TMP/$1/pkg.tar.gz"
    printf '%s' "$TMP/$1/pkg.tar.gz"
}

verdict() {
    if verify_checksum "$1" "$2" > /dev/null 2>&1; then echo installs; else echo refuses; fi
}

printf '%s  pkg.tar.gz\n' "$good" > "$TMP/remote/valid.sha256"
check "valid checksum installs" "installs" \
    "$(verdict "$(staged valid)" "file://$TMP/remote/valid.sha256")"

printf '%s  pkg.tar.gz\n' "$(printf '%064d' 0)" > "$TMP/remote/wrong.sha256"
check "mismatch refuses" "refuses" \
    "$(verdict "$(staged wrong)" "file://$TMP/remote/wrong.sha256")"

check "missing checksum refuses" "refuses" \
    "$(verdict "$(staged missing)" "file://$TMP/remote/absent.sha256")"

printf 'not-a-digest  pkg.tar.gz\n' > "$TMP/remote/malformed.sha256"
check "malformed checksum refuses" "refuses" \
    "$(verdict "$(staged malformed)" "file://$TMP/remote/malformed.sha256")"

: > "$TMP/remote/empty.sha256"
check "empty checksum refuses" "refuses" \
    "$(verdict "$(staged empty)" "file://$TMP/remote/empty.sha256")"

mkdir -p "$TMP/nobin"
check "no digest tool refuses" "refuses" \
    "$(PATH="$TMP/nobin" verdict "$(staged notool)" "file://$TMP/remote/valid.sha256")"

check "explicit override installs" "installs" \
    "$(BNKSOUND_SKIP_CHECKSUM=1 verdict "$(staged override)" "file://$TMP/remote/absent.sha256")"

cd "$ROOT"
if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "installer checks passed"
