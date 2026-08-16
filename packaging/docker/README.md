# Docker

```sh
docker compose -f packaging/docker/compose.yaml up --build
```

The image runs as UID/GID `65532` unless Compose overrides `PUID` and `PGID`. Its root filesystem is read-only, all Linux capabilities are dropped, and `/config` plus `/data` are durable. `COVALENT_BACKUP_SOURCE` is mounted read-only at `/source`. `COVALENT_RESTORE_TARGET` is the only example writable restore mount.

LAN discovery defaults off. Bridge networking exposes Covalent ports explicitly. Host networking or a Tailscale sidecar can improve discovery/routing, but neither is required and neither grants trust.

The HTTP API and authenticated QUIC transport both bind container interfaces on TCP/UDP 8787. The local API still requires the bearer token stored under `/data`; QUIC additionally requires a confirmed peer identity and pinned certificate.
