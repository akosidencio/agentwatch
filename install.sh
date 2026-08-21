#!/bin/sh
#
# AgentWatch installer.
#
#   curl -fsSL https://github.com/akosidencio/agentwatch/releases/latest/download/install.sh | sh
#
# Downloads the release archive for this machine, verifies it against the
# published SHA256SUMS, and puts three binaries on your PATH. It does not touch
# your Claude Code settings and does not install a service: both are separate,
# explicit commands that show you what they will do first.
#
# Environment:
#   AGENTWATCH_VERSION   tag to install, e.g. v0.1.0   (default: latest)
#   AGENTWATCH_BIN_DIR   install directory             (default: ~/.local/bin)

set -eu

REPO="akosidencio/agentwatch"
VERSION="${AGENTWATCH_VERSION:-latest}"
BIN_DIR="${AGENTWATCH_BIN_DIR:-$HOME/.local/bin}"
BINARIES="agentwatch agentwatch-daemon agentwatch-hook"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required but was not found"
}

need curl
need tar
need shasum

# --- what are we installing onto ---------------------------------------------

os="$(uname -s)"
[ "$os" = "Darwin" ] || die "AgentWatch supports macOS only; this is $os"

case "$(uname -m)" in
    arm64|aarch64) target="aarch64-apple-darwin" ;;
    x86_64)        target="x86_64-apple-darwin" ;;
    *)             die "unsupported architecture: $(uname -m)" ;;
esac

if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$VERSION"
fi

archive="agentwatch-$target.tar.gz"

say "AgentWatch installer"
say "  version:  $VERSION"
say "  target:   $target"
say "  into:     $BIN_DIR"
say ""

# --- download -----------------------------------------------------------------

tmp="$(mktemp -d)"
# Leave nothing behind, including on failure or interrupt.
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$archive" "$base/$archive" \
    || die "could not download $base/$archive (no release published for this version yet?)"

say "downloading SHA256SUMS"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" \
    || die "could not download $base/SHA256SUMS"

# --- verify -------------------------------------------------------------------
#
# Checked before anything is written to PATH, not after. A corrupted or
# substituted archive should never reach the point of being executable.

say "verifying checksum"
expected="$(grep " .*${archive}\$" "$tmp/SHA256SUMS" | awk '{print $1}' | head -n1)"
[ -n "$expected" ] || die "$archive is not listed in SHA256SUMS"

actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
if [ "$expected" != "$actual" ]; then
    say "  expected: $expected"
    say "  actual:   $actual"
    die "checksum mismatch; refusing to install"
fi
say "  ok ($actual)"

# --- install ------------------------------------------------------------------

tar -xzf "$tmp/$archive" -C "$tmp"

for binary in $BINARIES; do
    [ -f "$tmp/$binary" ] || die "$archive did not contain $binary"
done

mkdir -p "$BIN_DIR" || die "could not create $BIN_DIR"

for binary in $BINARIES; do
    # Replace by rename so a running daemon keeps its open inode rather than
    # having its file rewritten underneath it.
    chmod 755 "$tmp/$binary"
    mv -f "$tmp/$binary" "$BIN_DIR/$binary" \
        || die "could not write to $BIN_DIR (try AGENTWATCH_BIN_DIR=/somewhere/writable)"
    say "installed $BIN_DIR/$binary"
done

installed="$("$BIN_DIR/agentwatch" --version 2>/dev/null || true)"
[ -n "$installed" ] || die "$BIN_DIR/agentwatch did not run after installation"
say ""
say "$installed"

# --- what to do next ----------------------------------------------------------

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        say ""
        say "NOTE: $BIN_DIR is not on your PATH. Add this to your shell profile:"
        say ""
        say "  export PATH=\"\$PATH:$BIN_DIR\""
        ;;
esac

cat <<'NEXT'

Next steps:

  agentwatch install-hooks    # register the hooks; shows the diff and asks first
  agentwatch service install  # run the daemon at login
  agentwatch import           # read the history Claude Code has already written

Hooks are read at session start, so open a new agent session afterwards.
NEXT
