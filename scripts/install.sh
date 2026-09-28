#!/bin/sh
# Install or upgrade native Codex Swap (xswap) on macOS or Linux without Rust, Cargo,
# Homebrew, or administrator access.
#
#   curl -fsSL https://github.com/maddada/codex-swap/releases/latest/download/install.sh | sh
#
# Environment:
#   XSWAP_VERSION      Install this release (MAJOR.MINOR.PATCH, optional v prefix) instead of the latest.
#   XSWAP_INSTALL_DIR  Directory for the xswap executable (default: $HOME/.local/bin).
#
# The executable is checked against the release's SHA256SUMS and its own --version before it replaces
# anything. LICENSE and THIRD_PARTY_NOTICES.md go to ${XDG_DATA_HOME:-$HOME/.local/share}/doc/codex-swap.
# A receipt, .xswap-install-receipt.json, is written beside the executable so `xswap upgrade` (and tools
# such as Ghostex) can recognise this installation and rerun this script for the same directory.
# Shell profiles are never edited. Runs without a terminal: nothing here reads from stdin.
set -eu

REPOSITORY="maddada/codex-swap"
RECEIPT_NAME=".xswap-install-receipt.json"

fail() {
    printf 'codex-swap install failed: %s\n' "$*" >&2
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

[ -n "${HOME:-}" ] || fail "HOME is not set."

if have curl; then
    fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif have wget; then
    fetch() { wget -q -O "$2" "$1"; }
else
    fail "curl or wget is required to download the release."
fi

if have sha256sum; then
    sha256() { sha256sum "$1" | awk '{print $1}'; }
elif have shasum; then
    sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
elif have openssl; then
    sha256() { openssl dgst -sha256 "$1" | awk '{print $NF}'; }
else
    fail "sha256sum, shasum or openssl is required to verify the download."
fi

have tar || fail "tar is required to unpack the release."

os=$(uname -s)
arch=$(uname -m)
case "$os" in
Darwin)
    major=$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)
    if [ -n "$major" ] && [ "$major" -lt 11 ] 2>/dev/null; then
        fail "macOS 11 or newer is required."
    fi
    case "$arch" in
    arm64 | aarch64) target=aarch64-apple-darwin ;;
    x86_64)
        # A shell running under Rosetta reports x86_64 on Apple Silicon; install the native build.
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
            target=aarch64-apple-darwin
        else
            target=x86_64-apple-darwin
        fi
        ;;
    *) fail "Unsupported macOS architecture: $arch." ;;
    esac
    ;;
Linux)
    case "$arch" in
    aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
    x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
    *) fail "Unsupported Linux architecture: $arch. x86-64 or ARM64 is required." ;;
    esac
    ;;
*) fail "Unsupported operating system: $os. On Windows use install.ps1." ;;
esac

install_dir=${XSWAP_INSTALL_DIR:-"$HOME/.local/bin"}
case "$install_dir" in
/*) ;;
*) install_dir="$(pwd)/$install_dir" ;;
esac
case "$install_dir" in
*'
'*) fail "The installation directory must not contain a newline." ;;
esac

data_home=${XDG_DATA_HOME:-}
case "$data_home" in
/*) ;;
*) data_home="$HOME/.local/share" ;;
esac
doc_dir="$data_home/doc/codex-swap"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t codex-swap) || fail "could not create a temporary directory."
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' HUP INT TERM

# The checksum list names every archive with its version, so reading it from the latest release tells
# us the version without the rate-limited GitHub API. Archives then come from that exact tag, so a
# release published mid-install cannot mix versions.
if [ -n "${XSWAP_VERSION:-}" ]; then
    requested=${XSWAP_VERSION#v}
    printf '%s\n' "$requested" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
        fail "XSWAP_VERSION must be MAJOR.MINOR.PATCH, optionally prefixed with v."
    sums_url="https://github.com/$REPOSITORY/releases/download/v$requested/SHA256SUMS"
else
    sums_url="https://github.com/$REPOSITORY/releases/latest/download/SHA256SUMS"
fi
fetch "$sums_url" "$tmp/SHA256SUMS" || fail "could not download the release checksums from $sums_url."

matches=$(grep -E "^[0-9a-fA-F]{64}  codex-swap-[0-9]+\.[0-9]+\.[0-9]+-$target\.tar\.gz$" "$tmp/SHA256SUMS" || true)
[ -n "$matches" ] || fail "the release has no $target build."
[ "$(printf '%s\n' "$matches" | wc -l | tr -d ' ')" = 1 ] ||
    fail "the release checksum list has more than one $target build."
expected=$(printf '%s\n' "$matches" | cut -c1-64 | tr 'A-F' 'a-f')
archive_name=$(printf '%s\n' "$matches" | cut -c67-)
version=$(printf '%s\n' "$archive_name" | sed -e 's/^codex-swap-//' -e "s/-$target\.tar\.gz\$//")
if [ -n "${XSWAP_VERSION:-}" ] && [ "$version" != "${XSWAP_VERSION#v}" ]; then
    fail "release v${XSWAP_VERSION#v} lists version $version."
fi

archive="$tmp/$archive_name"
fetch "https://github.com/$REPOSITORY/releases/download/v$version/$archive_name" "$archive" ||
    fail "could not download $archive_name."
[ "$(sha256 "$archive")" = "$expected" ] || fail "SHA-256 verification failed. Nothing was installed."

members=$(tar -tzf "$archive" | LC_ALL=C sort | tr '\n' ' ')
[ "$members" = "LICENSE README.md THIRD_PARTY_NOTICES.md xswap " ] ||
    fail "unexpected release archive contents. Nothing was installed."
tar -tvzf "$archive" | cut -c1 | grep -qv -- '-' &&
    fail "the release archive must contain only regular files. Nothing was installed."
mkdir "$tmp/extract"
tar -xzf "$archive" -C "$tmp/extract" xswap LICENSE THIRD_PARTY_NOTICES.md ||
    fail "could not unpack $archive_name."
chmod 755 "$tmp/extract/xswap"
reported=$("$tmp/extract/xswap" --version 2>/dev/null || true)
[ "$reported" = "xswap $version" ] || fail "the downloaded executable did not pass its version check."
binary_sha256=$(sha256 "$tmp/extract/xswap")

mkdir -p "$install_dir" || fail "could not create $install_dir."
mkdir -p "$doc_dir" || fail "could not create $doc_dir."
cp "$tmp/extract/LICENSE" "$tmp/extract/THIRD_PARTY_NOTICES.md" "$doc_dir/" ||
    fail "could not write the license documents to $doc_dir."

# Stage beside the destination so the final rename is atomic; a running old xswap keeps its inode.
staged="$install_dir/.xswap.$$.new"
cp "$tmp/extract/xswap" "$staged" || fail "could not write to $install_dir."
chmod 755 "$staged"
mv -f "$staged" "$install_dir/xswap" || {
    rm -f "$staged"
    fail "could not replace $install_dir/xswap."
}

json_string() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}
receipt="$install_dir/$RECEIPT_NAME"
cat >"$receipt.$$.new" <<EOF
{"schemaVersion":1,"method":"script","installer":"https://github.com/$REPOSITORY/releases/latest/download/install.sh","installDir":"$(json_string "$install_dir")","docDir":"$(json_string "$doc_dir")","version":"$version","target":"$target","sha256":"$binary_sha256"}
EOF
mv -f "$receipt.$$.new" "$receipt" || fail "could not write $receipt."

printf 'Installed xswap %s to %s/xswap\n' "$version" "$install_dir"
case ":${PATH:-}:" in
*":$install_dir:"*)
    first=$(command -v xswap 2>/dev/null || true)
    if [ -n "$first" ] && [ "$first" != "$install_dir/xswap" ]; then
        printf 'Note: %s comes first on PATH and hides this installation.\n' "$first"
    fi
    ;;
*) printf 'Note: %s is not on PATH. Add it to PATH to run xswap.\n' "$install_dir" ;;
esac
printf 'Install the official Codex CLI separately.\n'
