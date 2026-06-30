#!/usr/bin/env bash
#
# Dev deploy helper: build agenta and install BOTH binaries to ALL install
# locations, then cleanly restart the daemon.
#
# Encapsulates the footguns that bite a manual `cp`:
#   1. There are TWO binaries — `agenta` (CLI) and `agenta-daemon` (the daemon
#      that deserializes agent JSON). Deploying only one leaves stale behaviour.
#   2. Both `~/.local/bin` and `~/.cargo/bin` may be on PATH (an old
#      `cargo install` populates the latter). Update both so it can't matter
#      which one your shell resolves.
#   3. On macOS, copying a binary can invalidate its signature → "killed: 9".
#      Re-sign ad-hoc after each copy.
#
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

BINS=(agenta agenta-daemon)
DIRS=("$HOME/.local/bin" "$HOME/.cargo/bin")

echo "==> Building..."
cargo build

echo "==> Deploying binaries..."
for bin in "${BINS[@]}"; do
  src="target/debug/$bin"
  [[ -f "$src" ]] || { echo "ERROR: missing build artifact $src" >&2; exit 1; }
  for dir in "${DIRS[@]}"; do
    [[ -d "$dir" ]] || continue
    rm -f "$dir/$bin"
    cp "$src" "$dir/$bin"
    if [[ "$(uname)" == "Darwin" ]]; then
      codesign --force --sign - "$dir/$bin" 2>/dev/null || true
    fi
    echo "    -> $dir/$bin"
  done
done

echo "==> Restarting daemon..."
agenta daemon restart

echo "==> Done."
agenta daemon status || true
