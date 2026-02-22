#!/usr/bin/env bash
set -euo pipefail

REPO="pmbrull/claustre"
INSTALL_DIR="${CLAUSTRE_INSTALL_DIR:-$HOME/.local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

info()  { echo -e "${BOLD}${GREEN}==>${RESET} ${BOLD}$1${RESET}"; }
warn()  { echo -e "${YELLOW}warning:${RESET} $1"; }
error() { echo -e "${RED}error:${RESET} $1" >&2; exit 1; }

# Detect OS and architecture
detect_platform() {
  local os arch

  case "$(uname -s)" in
    Linux*)  os="linux" ;;
    Darwin*) os="macos" ;;
    *)       error "Unsupported OS: $(uname -s). Only Linux and macOS are supported." ;;
  esac

  case "$(uname -m)" in
    x86_64|amd64)  arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *)             error "Unsupported architecture: $(uname -m). Only x86_64 and aarch64 are supported." ;;
  esac

  echo "${os}-${arch}"
}

# Get latest release tag from GitHub API
get_latest_version() {
  local url="https://api.github.com/repos/${REPO}/releases/latest"
  if command -v curl &>/dev/null; then
    curl -fsSL "$url" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//'
  elif command -v wget &>/dev/null; then
    wget -qO- "$url" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//'
  else
    error "Neither curl nor wget found. Please install one of them."
  fi
}

# Download and install
install() {
  local platform version archive_name url tmp_dir

  platform="$(detect_platform)"
  info "Detected platform: ${platform}"

  info "Fetching latest release..."
  version="$(get_latest_version)"
  if [ -z "$version" ]; then
    error "Could not determine latest version. Check https://github.com/${REPO}/releases"
  fi
  info "Latest version: ${version}"

  archive_name="claustre-${platform}.tar.gz"
  url="https://github.com/${REPO}/releases/download/${version}/${archive_name}"

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  info "Downloading ${url}..."
  if command -v curl &>/dev/null; then
    curl -fsSL "$url" -o "${tmp_dir}/${archive_name}"
  else
    wget -q "$url" -O "${tmp_dir}/${archive_name}"
  fi

  info "Extracting..."
  tar -xzf "${tmp_dir}/${archive_name}" -C "$tmp_dir"

  # Install binary
  mkdir -p "$INSTALL_DIR"
  mv "${tmp_dir}/claustre" "${INSTALL_DIR}/claustre"
  chmod +x "${INSTALL_DIR}/claustre"

  info "Installed claustre to ${INSTALL_DIR}/claustre"

  # Check if install dir is in PATH
  if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    warn "${INSTALL_DIR} is not in your PATH."
    echo ""
    echo "  Add it to your shell profile:"
    echo ""
    echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
  fi

  echo ""
  info "Done! Run 'claustre' to launch the dashboard."
}

install
