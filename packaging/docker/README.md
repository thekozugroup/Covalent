# Docker

## Safe default

```sh
mkdir -p /srv/covalent/{config,data,source,restore}
docker compose -f packaging/docker/compose.yaml up --build
```

The image is rootless (`65532:65532` by default), has a read-only root filesystem, drops every Linux capability, enables `no-new-privileges`, and uses a `noexec,nosuid` temporary filesystem. `/data` is the durable encrypted engine state, identity, keys, and local API token. `/config` is a separate durable operator directory for exported safe settings. It is not a writable shortcut into a source share.

Docker selects ownership with `--user` / Compose `PUID` and `PGID`; it never starts as root or changes host ownership. `UMASK` accepts a three-digit octal value and defaults to `027`. Ensure both durable mounts are writable by the chosen UID/GID before startup.

`COVALENT_BACKUP_SOURCE` mounts one selected directory at `/source` read-only. Set it to a specific share, never `/mnt/user`. `COVALENT_RESTORE_TARGET` is the only writable example bind mount and appears at `/restore`. Preview first, choose a conflict policy, then explicitly authorize the signed plan in the console or API.

## Console and local API

Open `http://localhost:8787`. The responsive no-framework console implements Pair, Backup, Restore, and Settings against the daemon's real `/api/v1/*` routes. Status is public; changes require the token at `/data/local-api-token`:

```sh
docker compose -f packaging/docker/compose.yaml exec node cat /data/local-api-token
```

The console keeps this token in page memory only. Do not publish port 8787 to an untrusted network or put the token in shell history. Settings export excludes identity keys, backup keys, grants, and provider credentials. Importing a changed discovery preference requires a node restart before the discovery service changes.

## LAN and Tailscale

LAN discovery defaults to `false`. Bridge mode publishes TCP and UDP 8787 explicitly and works with manual addresses. Host networking may improve multicast discovery but exposes the local console on the host network; use it only after choosing appropriate firewall boundaries. A Tailscale sidecar or host Tailnet address can provide reachability without host networking. Neither discovery nor Tailscale establishes trust: each provider still needs confirmed pairing and a pinned certificate.

## Multi-architecture, reproducibility, and supply chain

Build the release image for both supported container architectures with Buildx:

```sh
docker buildx build --platform linux/amd64,linux/arm64 \
  -f packaging/docker/Dockerfile -t ghcr.io/thekozugroup/covalent:local --load .
```

The release workflow builds the same pinned Rust toolchain and Debian base for `linux/amd64` and `linux/arm64`, emits an SPDX SBOM, scans the image with Grype, and keylessly signs release-tag image digests with Cosign OIDC. Verification commands are recorded with the release artifact; a signature is not implied for local developer tags.

Run deterministic container checks locally:

```sh
docker build -f packaging/docker/Dockerfile -t covalent:e2e .
./scripts/check-container-runtime.sh covalent:e2e
./scripts/docker-compose-e2e.sh covalent:e2e
```

The Compose drill starts three rootless/read-only nodes, confirms pairing, selects two providers explicitly, backs up nested paths, loses the source, rejects local ciphertext corruption, repairs it from paired providers, imports safe settings, and restores only relative paths under `/restore`.
