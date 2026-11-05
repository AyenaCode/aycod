#!/bin/sh
# Installs the aycod binary from the GitHub release, after checking its SHA-256.
#
#   curl -fsSL https://github.com/AyenaCode/aycod/releases/latest/download/install.sh | sh
#
# Environment:
#   AYCOD_VERSION       release tag to install, e.g. v0.1.0 (default: the latest release)
#   AYCOD_INSTALL_DIR   where to put the binary (default: ~/.local/bin)
#
# Linux (x86_64, aarch64; static binary, any distribution) and macOS (Apple silicon, Intel).
set -eu

REPO="AyenaCode/aycod"
DIR="${AYCOD_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${AYCOD_VERSION:-latest}"

say() { printf 'aycod-install: %s\n' "$*"; }
die() { printf 'aycod-install: error: %s\n' "$*" >&2; exit 1; }

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) die "unsupported CPU $(uname -m): prebuilt binaries exist for x86_64 and aarch64 (or build from source)" ;;
esac
case "$(uname -s)" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin)
    # An x86_64 shell under Rosetta still gets the native Apple silicon binary.
    if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
      arch=aarch64
    fi
    target="$arch-apple-darwin"
    ;;
  *) die "unsupported OS $(uname -s): aycod runs on Linux and macOS (Windows: not yet)" ;;
esac

if [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

if command -v curl >/dev/null 2>&1; then
  fetch() { curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget --https-only -q -O "$2" "$1"; }
else
  die "curl or wget is required"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
  die "sha256sum or shasum is required to check the download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
archive="aycod-$target.tar.gz"

say "downloading $archive ($VERSION)"
fetch "$base/$archive" "$tmp/$archive" || die "download failed: $base/$archive"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"

expected="$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || die "$archive is not listed in SHA256SUMS"
actual="$(sha256 "$tmp/$archive")"
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual)"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$DIR"
# Copy then rename: a running aycod (an agent hook) is never left with a half-written file.
cp "$tmp/aycod-$target/aycod" "$DIR/.aycod.tmp"
chmod 755 "$DIR/.aycod.tmp"
mv -f "$DIR/.aycod.tmp" "$DIR/aycod"

say "installed $("$DIR/aycod" --version) to $DIR/aycod"
case ":$PATH:" in
  *":$DIR:"*) ;;
  *)
    say "$DIR is not on your PATH, and the git and agent hooks call \`aycod\` from PATH. Add it:"
    say "  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.profile   # then open a new shell"
    ;;
esac
say "next: cd your-project && aycod init --stack <sveltekit|rust|go|generic> && aycod install"
