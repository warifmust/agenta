#!/usr/bin/env bash
set -euo pipefail

# Agenta installer
# Supports:
# 1) Prebuilt binaries from GitHub Releases
# 2) Fallback to cargo install from GitHub

REPO="${AGENTA_REPO:-arifmustaffa/agenta}"
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
    x86_64|amd64) echo "amd64" ;;
    arm64|aarch64) echo "arm64" ;;
    *)
      echo "Error: unsupported architecture: $(uname -m)" >&2
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
  tag="$(curl -fsSL "$api" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  if [ -z "$tag" ]; then
    echo "Error: could not resolve latest release tag from $api" >&2
    exit 1
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
  local os arch version asset url tmp
  os="$(detect_os)"
  arch="$(detect_arch)"
  version="$(resolve_version)"
  asset="agenta-${os}-${arch}.tar.gz"
  url="https://github.com/${REPO}/releases/download/${version}/${asset}"

  echo "Installing agenta ${version} from ${url}"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  need_cmd curl
  need_cmd tar
  curl -fL "$url" -o "${tmp}/${asset}"
  tar -xzf "${tmp}/${asset}" -C "$tmp"

  ensure_install_dir
  install -m 0755 "${tmp}/agenta" "${INSTALL_DIR}/agenta"
  install -m 0755 "${tmp}/agenta-daemon" "${INSTALL_DIR}/agenta-daemon"

  echo "Installed:"
  echo "  - ${INSTALL_DIR}/agenta"
  echo "  - ${INSTALL_DIR}/agenta-daemon"
}

install_from_cargo() {
  need_cmd cargo
  local git_ref=""
  if [ "$VERSION" != "latest" ]; then
    git_ref="--tag ${VERSION}"
  fi
  echo "Falling back to cargo install from https://github.com/${REPO}.git"
  # shellcheck disable=SC2086
  cargo install --git "https://github.com/${REPO}.git" ${git_ref} --locked --force
}

main() {
  if install_from_release; then
    exit 0
  fi
  install_from_cargo
}

main "$@"
