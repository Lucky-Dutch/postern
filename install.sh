#!/bin/sh
# Installs Postern from this repository in one step: checks the Xcode command
# line tools, installs Rust when it is missing, builds the app, puts it in
# Applications, and opens it.
#
#   ./install.sh            build, install, and open Postern
#   ./install.sh --no-open  build and install only
set -eu
cd "$(dirname "$0")"

open_app=true
case "${1:-}" in
  --no-open) open_app=false ;;
  "") ;;
  *) printf 'usage: %s [--no-open]\n' "$0" >&2; exit 2 ;;
esac

step() { printf '\n\033[1;34m==>\033[0m \033[1m%s\033[0m\n' "$1"; }
fail() { printf '\n\033[1;31mError:\033[0m %s\n' "$1" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || fail "Postern runs on macOS."
major=$(sw_vers -productVersion | cut -d. -f1)
[ "$major" -ge 13 ] || fail "Postern needs macOS 13 or later; this Mac runs $(sw_vers -productVersion)."

step "Checking the Xcode command line tools"
if ! xcode-select -p >/dev/null 2>&1; then
  xcode-select --install >/dev/null 2>&1 || true
  fail "Install the command line tools in the window that just opened, then run ./install.sh again."
fi
echo "Found $(xcode-select -p)"

step "Checking Rust"
if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
if ! command -v rustup >/dev/null 2>&1; then
  echo "Rust is not installed. Installing rustup, the official installer, from https://rustup.rs"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
  . "$HOME/.cargo/env"
fi
# Installs the version pinned in rust-toolchain.toml, with rustfmt and clippy.
rustup toolchain install

step "Building and installing Postern (the first build takes a few minutes)"
sh scripts/bundle-macos.sh --install

if pgrep -x postern >/dev/null 2>&1; then
  printf '\nPostern is running. Quit it and open it again to use the new version.\n'
elif [ "$open_app" = true ]; then
  step "Opening Postern"
  open -b dev.postern.app
fi
printf '\nDone. To keep Postern in the Dock, right-click its icon and choose Options > Keep in Dock.\n'
