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

  ensure_install_dir
  install -m 0755 "${tmp}/agenta" "${INSTALL_DIR}/agenta"
  install -m 0755 "${tmp}/agenta-daemon" "${INSTALL_DIR}/agenta-daemon"
  rm -rf "$tmp"

  echo "Installed:"
  echo "  - ${INSTALL_DIR}/agenta"
  echo "  - ${INSTALL_DIR}/agenta-daemon"
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
  local git_ref=""
  if [ "$VERSION" != "latest" ]; then
    git_ref="--tag ${VERSION}"
  fi
  echo "Falling back to cargo install from https://github.com/${REPO}.git"
  # shellcheck disable=SC2086
  cargo install --git "https://github.com/${REPO}.git" ${git_ref} --locked --force
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

main() {
  if install_from_release; then
    bootstrap
    return 0
  fi
  echo "Info: release binary not available, using cargo install fallback."
  install_from_cargo
  bootstrap
}

main "$@"
