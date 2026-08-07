#!/usr/bin/env sh
# Installs the `zhao-dbt-plan` binary for the current platform.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/allenhori/zhao-dbt-plan/master/scripts/install.sh | sh
#
# Environment variables:
#   ZHAO_DBT_PLAN_VERSION     A release tag to install (e.g. "v0.1.0" or
#                             "nightly"). Defaults to "latest".
#   ZHAO_DBT_PLAN_INSTALL_DIR Where to place the binary. Defaults to
#                             "$HOME/.zhao/bin" -- the same directory
#                             zhao-cli itself installs to, so a user who
#                             already has that on PATH gets this addon
#                             auto-discovered as `zhao dbt-plan` with no
#                             extra PATH setup (see zhao-cli's Addon
#                             discovery convention).
#
# This script only downloads and unpacks a pre-built binary from
# https://github.com/allenhori/zhao-dbt-plan/releases -- no Rust
# toolchain needed, no elevated privileges asked for.

set -eu

REPO="allenhori/zhao-dbt-plan"
VERSION="${ZHAO_DBT_PLAN_VERSION:-latest}"
INSTALL_DIR="${ZHAO_DBT_PLAN_INSTALL_DIR:-$HOME/.zhao/bin}"

say() { printf '%s\n' "$1"; }
die() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      case "$arch" in
        arm64) echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) die "unsupported macOS architecture: $arch" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64) echo "x86_64-unknown-linux-gnu" ;;
        *) die "unsupported Linux architecture: $arch (only x86_64 has a released binary today -- build from source instead: cargo install --git https://github.com/$REPO)" ;;
      esac
      ;;
    *)
      die "unsupported OS: $os (Windows users: download zhao-dbt-plan-x86_64-pc-windows-msvc.zip directly from https://github.com/$REPO/releases)"
      ;;
  esac
}

main() {
  command -v curl >/dev/null 2>&1 || die "curl is required"
  command -v tar >/dev/null 2>&1 || die "tar is required"

  target="$(detect_target)"
  say "Detected platform: $target"

  if [ "$VERSION" = "latest" ]; then
    url="https://github.com/$REPO/releases/latest/download/zhao-dbt-plan-$target.tar.gz"
  else
    url="https://github.com/$REPO/releases/download/$VERSION/zhao-dbt-plan-$target.tar.gz"
  fi

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  say "Downloading $url"
  curl -fsSL "$url" -o "$tmp_dir/zhao-dbt-plan.tar.gz" \
    || die "download failed -- check that $VERSION is a real release tag at https://github.com/$REPO/releases"

  tar -xzf "$tmp_dir/zhao-dbt-plan.tar.gz" -C "$tmp_dir"

  mkdir -p "$INSTALL_DIR"
  mv "$tmp_dir/zhao-dbt-plan" "$INSTALL_DIR/zhao-dbt-plan"
  chmod +x "$INSTALL_DIR/zhao-dbt-plan"

  say "Installed zhao-dbt-plan to $INSTALL_DIR/zhao-dbt-plan"

  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      say ""
      say "$INSTALL_DIR isn't on your PATH yet. Add it, e.g.:"
      say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc   # or ~/.zshrc"
      ;;
  esac

  say ""
  say "Run 'zhao-dbt-plan --help' to get started (after adding it to your PATH, or via"
  say "$INSTALL_DIR/zhao-dbt-plan directly). If zhao-cli is also installed and this addon"
  say "is on the same PATH, 'zhao dbt-plan' works too."
}

main
