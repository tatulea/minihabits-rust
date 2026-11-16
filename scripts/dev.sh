#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export MINIHABITS_ORIGIN=http://localhost:5175
export LEPTOS_SITE_ADDR=127.0.0.1:5175
export TZ="${TZ:-UTC}"
exec cargo leptos watch
