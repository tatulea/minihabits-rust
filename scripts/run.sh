#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Default to local HTTP development when no browser origin is configured.
if [[ -z "${MINIHABITS_ORIGIN:-}" ]]; then
  export MINIHABITS_ORIGIN=http://localhost:5175
  export LEPTOS_SITE_ADDR=127.0.0.1:5175
fi
export TZ="${TZ:-UTC}"
exec "${MINIHABITS_BINARY:-./target/debug/minihabits-rust}" "$@"
