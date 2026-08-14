#!/usr/bin/env bash
# arrano installer — fetches the latest GitHub release binary.
#   curl -fsSL https://raw.githubusercontent.com/enekos/arrano/master/install.sh | bash
# Env: ARRANO_INSTALL_DIR overrides the install location (default ~/.local/bin).
set -euo pipefail

REPO="enekos/arrano"
BIN="arrano"

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    case "$arch" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) echo "error: unsupported macOS arch: $arch" >&2; exit 1 ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-gnu" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
      *) echo "error: unsupported Linux arch: $arch" >&2; exit 1 ;;
    esac ;;
  *) echo "error: unsupported OS: $os" >&2; exit 1 ;;
esac

url="https://github.com/$REPO/releases/latest/download/$BIN-$target.tar.gz"
dir="${ARRANO_INSTALL_DIR:-$HOME/.local/bin}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "→ downloading $url"
curl -fsSL "$url" -o "$tmp/$BIN.tar.gz"
tar -xzf "$tmp/$BIN.tar.gz" -C "$tmp"

mkdir -p "$dir"
install -m 755 "$tmp/$BIN" "$dir/$BIN"
echo "→ installed $("$dir/$BIN" --version) to $dir/$BIN"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH — add: export PATH=\"$dir:\$PATH\"" ;;
esac
