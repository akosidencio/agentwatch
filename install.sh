#!/bin/sh
#
# AgentWatch installer.
#
#   curl -fsSL https://github.com/akosidencio/agentwatch/releases/latest/download/install.sh | sh
#
# Downloads the release archive for this machine, verifies it against the
# published SHA256SUMS, and puts it on your PATH. Then it tells you to run
# `agentwatch init`, which is the one command that sets everything up.
#
# This script does not touch your Claude Code settings and does not install a
# service. `init` does both, and shows you the diff and the job definition
# before it writes anything.
#
# Environment:
#   AGENTWATCH_VERSION          tag to install, e.g. v0.2.0  (default: latest)
#   AGENTWATCH_BIN_DIR          install directory            (default: ~/.local/bin)
#   AGENTWATCH_NO_MODIFY_PATH   set to 1 to leave your shell profile alone

set -eu

REPO="akosidencio/agentwatch"
VERSION="${AGENTWATCH_VERSION:-latest}"
BIN_DIR="${AGENTWATCH_BIN_DIR:-$HOME/.local/bin}"
# The executable, and anything else the archive happens to carry. Only the
# first is required; the rest are installed if present, so adding or dropping a
# companion binary in a release does not need a new installer.
REQUIRED="agentwatch"
OPTIONAL="agentwatch-menubar"

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

for binary in $REQUIRED; do
    [ -f "$tmp/$binary" ] || die "$archive did not contain $binary"
done

mkdir -p "$BIN_DIR" || die "could not create $BIN_DIR"

for binary in $REQUIRED $OPTIONAL; do
    [ -f "$tmp/$binary" ] || continue
    # Replace by rename so a running daemon keeps its open inode rather than
    # having its file rewritten underneath it.
    chmod 755 "$tmp/$binary"
    mv -f "$tmp/$binary" "$BIN_DIR/$binary" \
        || die "could not write to $BIN_DIR (try AGENTWATCH_BIN_DIR=/somewhere/writable)"
    say "installed $BIN_DIR/$binary"
done

# Binaries from 0.1 are deliberately left alone here. They are dead weight now,
# but one of them is what the installed launchd job still runs: removing it
# before `init` has rewritten that job would stop collection. `init` cleans them
# up, in that order.

installed="$("$BIN_DIR/agentwatch" --version 2>/dev/null || true)"
[ -n "$installed" ] || die "$BIN_DIR/agentwatch did not run after installation"

# --- PATH ---------------------------------------------------------------------
#
# A printed warning is the wrong tool here: in a `curl | sh` pipe it scrolls
# past, and the very next thing the user is told to type is a command that will
# not resolve. So the profile is edited, with a marker line saying who did it
# and how to opt out. Set AGENTWATCH_NO_MODIFY_PATH=1 to be left alone.

MARKER="# added by the AgentWatch installer"

# The profile the user's own login shell reads. $SHELL rather than $0: this
# script runs under sh regardless of what the user actually uses.
case "${SHELL##*/}" in
    zsh)  profile="$HOME/.zshrc" ;;
    bash) if [ -f "$HOME/.bash_profile" ]; then
              profile="$HOME/.bash_profile"
          else
              profile="$HOME/.bashrc"
          fi ;;
    fish) profile="$HOME/.config/fish/config.fish" ;;
    *)    profile="" ;;
esac

# $HOME left unexpanded where it applies, so the line stays correct if the home
# directory is ever mounted somewhere else.
case "$BIN_DIR" in
    "$HOME"/*) path_line="export PATH=\"\$PATH:\$HOME${BIN_DIR#"$HOME"}\"" ;;
    *)         path_line="export PATH=\"\$PATH:$BIN_DIR\"" ;;
esac
[ "${SHELL##*/}" = "fish" ] && path_line="fish_add_path $BIN_DIR"

on_path=no
case ":$PATH:" in *":$BIN_DIR:"*) on_path=yes ;; esac

if [ "$on_path" = "yes" ]; then
    :
elif [ "${AGENTWATCH_NO_MODIFY_PATH:-0}" = "1" ]; then
    say ""
    say "NOTE: $BIN_DIR is not on your PATH. Add this yourself:"
    say ""
    say "  $path_line"
elif [ -z "$profile" ]; then
    say ""
    say "NOTE: $BIN_DIR is not on your PATH, and I do not know which profile"
    say "      ${SHELL:-your shell} reads. Add this to it:"
    say ""
    say "  $path_line"
elif grep -Fq "$MARKER" "$profile" 2>/dev/null; then
    # Already added, and still not on PATH: the profile has not been reloaded.
    say ""
    say "NOTE: $profile already adds $BIN_DIR, but this shell has not read it yet."
else
    mkdir -p "$(dirname "$profile")" 2>/dev/null || true
    {
        printf '\n%s\n' "$MARKER"
        printf '%s\n' "$path_line"
    } >> "$profile" || die "could not append to $profile"
    say ""
    say "added $BIN_DIR to your PATH in $profile"
    say "(AGENTWATCH_NO_MODIFY_PATH=1 skips that)"
fi

# --- welcome ------------------------------------------------------------------
#
# Printed by running the binary that was just installed, rather than echoed from
# here: one definition of the greeting, and it doubles as proof that what landed
# on disk actually runs. A bare invocation is the welcome screen — the banner,
# the version, the state, and what to type next.

say ""
"$BIN_DIR/agentwatch" || die "$BIN_DIR/agentwatch did not run after installation"

# The one command to type next, spelled absolutely: the profile edit above does
# not affect the shell reading this, so a bare `agentwatch` may not resolve yet.
if [ "$on_path" != "yes" ]; then
    say ""
    say "  This shell has not picked up your PATH yet, so start with:"
    say ""
    say "      $BIN_DIR/agentwatch init"
    say ""
    say "  In a new terminal, plain \`agentwatch\` works."
fi
