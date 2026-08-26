#!/usr/bin/env bash
# Build the upstream xai-grok-pager release binary and expose it as `grog`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$root"

pager="target/release/xai-grok-pager"
grog="target/release/grog"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found on PATH" >&2
  exit 1
fi

echo "Building xai-grok-pager (release)"
cargo build -p xai-grok-pager-bin --release

if [[ ! -f "$pager" || ! -x "$pager" ]]; then
  echo "error: expected executable ${pager}" >&2
  exit 1
fi

ln -sfn xai-grok-pager "$grog"

if [[ ! -L "$grog" ]]; then
  echo "error: failed to create ${grog} -> xai-grok-pager" >&2
  exit 1
fi

echo "Ready: ${grog} -> xai-grok-pager"

grog_bin_dir="${GROG_BIN_DIR:-${HOME:+$HOME/.local/bin}}"
if [[ -z "$grog_bin_dir" ]]; then
  echo "error: install directory is empty (set GROG_BIN_DIR, or HOME for ~/.local/bin)" >&2
  exit 1
fi

mkdir -p "$grog_bin_dir"
user_grog="${grog_bin_dir}/grog"
ln -sfn "$root/$grog" "$user_grog"

if [[ ! -L "$user_grog" ]]; then
  echo "error: failed to create ${user_grog} -> ${root}/${grog}" >&2
  exit 1
fi

echo "Installed: ${user_grog} -> ${root}/${grog}"
