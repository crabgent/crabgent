#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

(
  cd "$root/app"
  CARGO_TARGET_DIR="$target_dir" cargo build --release --bin crabgent
)

binary="$target_dir/release/crabgent"
if [[ "$(uname -s)" == "Darwin" ]] && command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$binary" >/dev/null
fi

echo "$binary"
