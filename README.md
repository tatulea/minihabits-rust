# MiniHabits

A habit tracker built with Rust, Leptos, Axum, and SQLite. Set a minimum and full version of each habit, record daily check-ins, and review your week.

This is a Rust rewrite of [fberrez/minihabits-archive](https://github.com/fberrez/minihabits-archive), which is no longer maintained. It is based on commit [`fd93c85d4e497b81309807e5b6929d81378befa3`](https://github.com/fberrez/minihabits-archive/tree/fd93c85d4e497b81309807e5b6929d81378befa3).

## Run locally

From the project root:

```sh
cargo install cargo-leptos --version 0.3.12 --locked
cargo leptos build
./scripts/run.sh
```

Open [http://localhost:5175](http://localhost:5175). Use `localhost`, since the app checks the exact hostname. HTTP works automatically with a local origin and loopback listener.

For live reload, run `./scripts/dev.sh`.

Data defaults to `data/minihabits.sqlite3`. Set `MINIHABITS_DATABASE` to change the path and `TZ` to choose an IANA time zone, such as `Europe/Bucharest`. The default is UTC.

## Docker

After the workflow succeeds, pull and run the image with a proxy on the same host:

```sh
docker pull ghcr.io/tatulea/minihabits-rust:latest
docker run -d --name minihabits --restart unless-stopped \
  -p 127.0.0.1:5175:5175 \
  -v minihabits-data:/data \
  -e MINIHABITS_ORIGIN=https://habits.example.com \
  -e TZ=Europe/Bucharest \
  ghcr.io/tatulea/minihabits-rust:latest
```

The image runs as UID/GID `10001:10001`. A named volume persists accounts, habits, and sessions across container updates. If you use a host directory instead, make it writable by that UID/GID. On first launch, setup creates the administrator; afterward, only sign-in is shown.

## Hosting and security

MiniHabits is intended for personal or private LAN use. Run one app instance with SQLite on local disk.

The Rust server serves HTTP. For deployment, use an HTTPS reverse proxy and set `MINIHABITS_ORIGIN` to the exact browser origin. An `https://` origin automatically enables Secure session cookies. Preserve the Host header and keep request bodies and cookies out of proxy logs.

Keep the app on loopback with a proxy on the same host. With a proxy in another Docker container, set `LEPTOS_SITE_ADDR=0.0.0.0:5175`, share a private Docker network, and publish only the proxy's ports. Each client must trust the HTTPS certificate.

[MIT license](LICENSE).
