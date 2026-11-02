#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${CPR_PLUGIN_TARGET:-x86_64-unknown-linux-gnu}"
if [[ ! -f Cargo.lock ]]; then
  echo 'Cargo.lock does not exist. Run cargo generate-lockfile, inspect the result, then retry.' >&2
  exit 1
fi
cargo fmt
cargo test --locked
cargo build --release --locked --target "$TARGET"
if ! command -v cpr-plugin >/dev/null; then
  echo 'cpr-plugin CLI is required; see README.md (backend/apps/plugin-cli).' >&2
  exit 1
fi
cpr-plugin package \
  --manifest "$(pwd)/plugin.json" \
  --binary "$(pwd)/target/$TARGET/release/cpr-image-bridge" \
  --target "$TARGET" \
  --output-dir "$(pwd)/dist"
