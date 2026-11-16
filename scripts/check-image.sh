#!/usr/bin/env bash
set -euo pipefail

image="${1:?Usage: scripts/check-image.sh IMAGE}"
name="minihabits-check-$$"
volume="${name}-data"
scratch="$(mktemp -d)"

cleanup() {
  status=$?
  trap - EXIT
  if [[ "$status" -ne 0 ]]; then
    docker logs "$name" >&2 || true
  fi
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
  rm -rf "$scratch"
  exit "$status"
}
trap cleanup EXIT

docker volume create "$volume" >/dev/null

start() {
  docker run --detach --name "$name" \
    --publish 127.0.0.1:15175:5175 \
    --mount "type=volume,source=$volume,target=/data" \
    --env MINIHABITS_ORIGIN=https://habits.test \
    "$image" >/dev/null
}

fetch_page() {
  curl --fail --silent --show-error \
    --retry 15 --retry-connrefused --retry-delay 1 --retry-max-time 45 \
    --connect-timeout 2 --max-time 10 \
    --header 'Host: habits.test' \
    http://127.0.0.1:15175/ --output "$scratch/page.html"
}

start
fetch_page
grep -q 'Set up MiniHabits' "$scratch/page.html"
[[ "$(docker exec "$name" id -u)" == 10001 ]]

for asset in app.css pkg/minihabits-rust.js pkg/minihabits-rust.wasm; do
  curl --fail --silent --show-error --connect-timeout 2 --max-time 10 \
    --header 'Host: habits.test' \
    "http://127.0.0.1:15175/$asset" --output /dev/null
done

# Exercise account creation and SQLite writes with throwaway CI credentials.
curl --fail --silent --show-error \
  --connect-timeout 2 --max-time 10 \
  --header 'Host: habits.test' \
  --header 'Origin: https://habits.test' \
  --data-urlencode 'username=image_check' \
  --data-urlencode 'password=synthetic-container-check-password' \
  http://127.0.0.1:15175/api/register --output "$scratch/account.json"
grep -Eq '"is_admin"[[:space:]]*:[[:space:]]*true' "$scratch/account.json"
docker exec "$name" test -s /data/minihabits.sqlite3

# A new container must find the account in the same named volume.
docker stop --time 10 "$name" >/dev/null
docker rm "$name" >/dev/null
start
fetch_page
grep -q 'Welcome back.' "$scratch/page.html"
if grep -q 'Set up MiniHabits' "$scratch/page.html"; then
  echo 'Setup reappeared after recreating the container.' >&2
  exit 1
fi
echo 'Image check passed: startup, assets, non-root user, account creation, and persistent data.'
