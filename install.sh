#!/usr/bin/env bash
set -euo pipefail

# Agenta installer
# Supports:
# 1) Prebuilt binaries from GitHub Releases
# 2) Fallback to cargo install from GitHub

REPO="${AGENTA_REPO:-warifmust/agenta}"
VERSION="${AGENTA_VERSION:-latest}"
INSTALL_DIR="${AGENTA_INSTALL_DIR:-/usr/local/bin}"

have_cmd() { command -v "$1" >/dev/null 2>&1; }

need_cmd() {
  if ! have_cmd "$1"; then
    echo "Error: required command not found: $1" >&2
    exit 1
  fi
}

detect_os() {
  case "$(uname -s)" in
    Darwin) echo "darwin" ;;
    Linux) echo "linux" ;;
    *)
      echo "Error: unsupported OS: $(uname -s)" >&2
      exit 1
      ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *)
      echo "Error: unsupported architecture: $(uname -m)" >&2
      exit 1
      ;;
  esac
}

# Returns the Rust target triple used by the CI release workflow.
detect_target() {
  local os arch
  os="$(detect_os)"
  arch="$(detect_arch)"
  case "${os}-${arch}" in
    darwin-aarch64)  echo "aarch64-apple-darwin" ;;
    darwin-x86_64)   echo "x86_64-apple-darwin" ;;
    linux-aarch64)   echo "aarch64-unknown-linux-gnu" ;;
    linux-x86_64)    echo "x86_64-unknown-linux-gnu" ;;
    *)
      echo "Error: unsupported platform: ${os}-${arch}" >&2
      exit 1
      ;;
  esac
}

resolve_version() {
  if [ "$VERSION" != "latest" ]; then
    echo "$VERSION"
    return
  fi

  need_cmd curl
  local api tag
  api="https://api.github.com/repos/${REPO}/releases/latest"
  tag="$(curl -fsSL "$api" 2>/dev/null | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  if [ -z "$tag" ]; then
    return 1
  fi
  echo "$tag"
}

ensure_install_dir() {
  if [ -w "$INSTALL_DIR" ]; then
    return
  fi

  INSTALL_DIR="${HOME}/.local/bin"
  mkdir -p "$INSTALL_DIR"
  echo "Info: using fallback install dir: $INSTALL_DIR"
  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) echo "Info: add this to your shell profile: export PATH=\"${INSTALL_DIR}:\$PATH\"" ;;
  esac
}

# Copy both binaries from $1 into the resolved install dir AND over whatever
# `agenta` currently resolves to on PATH (if different) — so an older copy earlier
# in PATH can't silently shadow an upgrade. This is the #1 cause of "cargo installed
# but `agenta --version` didn't change": cargo installs to ~/.cargo/bin while the
# on-PATH binary lives elsewhere.
place_binaries() {
  local src="$1"
  local -a dirs=("$INSTALL_DIR")
  local cur curdir
  cur="$(command -v agenta 2>/dev/null || true)"
  if [ -n "$cur" ]; then
    curdir="$(cd "$(dirname "$cur")" 2>/dev/null && pwd || true)"
    if [ -n "$curdir" ] && [ "$curdir" != "$INSTALL_DIR" ]; then
      dirs+=("$curdir")
    fi
  fi

  local d wrote=0
  for d in "${dirs[@]}"; do
    if install -m 0755 "${src}/agenta" "${d}/agenta" 2>/dev/null \
       && install -m 0755 "${src}/agenta-daemon" "${d}/agenta-daemon" 2>/dev/null; then
      echo "Installed: ${d}/agenta, ${d}/agenta-daemon"
      wrote=1
    else
      echo "Warning: could not write to ${d} (permissions?)." >&2
      echo "         Retry with: sudo install -m0755 ${src}/agenta ${d}/agenta" >&2
    fi
  done
  [ "$wrote" -eq 1 ]
}

install_from_release() {
  local target version asset url tmp
  target="$(detect_target)"
  version="$(resolve_version)" || return 1
  asset="agenta-${version}-${target}.tar.gz"
  url="https://github.com/${REPO}/releases/download/${version}/${asset}"

  echo "Installing agenta ${version} from ${url}"

  need_cmd curl
  need_cmd tar

  tmp="$(mktemp -d)"

  # Explicit cleanup on any failure — no trap (bash RETURN traps leak to caller)
  curl -fsSL "$url" -o "${tmp}/${asset}"    \
    && tar -xzf "${tmp}/${asset}" -C "$tmp" \
    && [ -f "${tmp}/agenta" ]               \
    && [ -f "${tmp}/agenta-daemon" ]        \
    || { rm -rf "$tmp"; return 1; }

  # The files existing isn't proof they RUN here: a glibc/ABI mismatch extracts
  # perfectly and then fails to start. Without this check we'd install a binary
  # that can't execute and never fall back to building from source. Returning 1
  # hands off to the cargo path instead of leaving a broken install.
  chmod +x "${tmp}/agenta" "${tmp}/agenta-daemon" 2>/dev/null || true
  if ! "${tmp}/agenta" --version >/dev/null 2>&1; then
    echo "Info: prebuilt binary for ${target} does not run on this system (ABI/glibc mismatch)." >&2
    rm -rf "$tmp"
    return 1
  fi

  ensure_install_dir
  place_binaries "$tmp" || { rm -rf "$tmp"; return 1; }
  rm -rf "$tmp"
}

ensure_cargo() {
  if have_cmd cargo; then
    return 0
  fi
  echo "" >&2
  echo "Error: cargo not found — Rust is required to build from source." >&2
  echo "" >&2
  echo "Install Rust via rustup (macOS and Linux):" >&2
  echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  echo "  source \$HOME/.cargo/env" >&2
  echo "" >&2
  echo "Then re-run this installer." >&2
  exit 1
}

install_from_cargo() {
  ensure_cargo
  ensure_install_dir
  local git_ref=""
  if [ "$VERSION" != "latest" ]; then
    git_ref="--tag ${VERSION}"
  fi
  echo "Falling back to cargo install from https://github.com/${REPO}.git"

  # Build into an isolated root (not ~/.cargo/bin) so the fresh binaries can be
  # placed into the SAME location the on-PATH `agenta` uses — otherwise cargo's
  # ~/.cargo/bin copy gets shadowed and the upgrade appears to do nothing.
  local tmp
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2086
  cargo install --git "https://github.com/${REPO}.git" ${git_ref} --locked --force --root "$tmp" \
    || { rm -rf "$tmp"; return 1; }
  place_binaries "${tmp}/bin" || { rm -rf "$tmp"; return 1; }
  rm -rf "$tmp"
}

bootstrap() {
  local bin="${INSTALL_DIR}/agenta"
  # Fallback to PATH if install dir binary not found
  if ! [ -x "$bin" ]; then
    bin="$(command -v agenta 2>/dev/null || true)"
  fi
  if [ -z "$bin" ] || ! [ -x "$bin" ]; then
    echo "Info: skipping bootstrap — agenta binary not found."
    return
  fi

  echo ""
  "$bin" setup
}

# Show what `agenta` actually resolves to now + its version, and warn if a copy
# outside the install dir still shadows PATH — so a botched upgrade is obvious.
report_installed() {
  local resolved
  resolved="$(command -v agenta 2>/dev/null || true)"
  if [ -z "$resolved" ]; then
    echo "Note: 'agenta' is not on your PATH yet. Add ${INSTALL_DIR} to PATH." >&2
    return
  fi
  echo ""
  echo "agenta resolves to: ${resolved}"
  "$resolved" --version 2>/dev/null || true
}

main() {
  if install_from_release; then
    report_installed
    bootstrap
    return 0
  fi
  echo "Info: release binary not available, using cargo install fallback."
  install_from_cargo
  report_installed
  bootstrap
}

main "$@"
