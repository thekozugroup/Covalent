# Unraid operation

Unraid is Tier 1. Install `packaging/unraid/covalent.xml` through Community Applications or Docker templates. It is intentionally unprivileged (`99:100`), read-only, capability-free, and uses `no-new-privileges` with a temporary filesystem. Do not enable privileged mode to solve mount permissions; make the selected host paths writable by the container identity instead.

## Required and optional mappings

| Container path | Unraid host path | Mode | Purpose |
| --- | --- | --- | --- |
| `/config` | `/mnt/user/appdata/covalent/config` | read/write | Operator-owned exported safe settings and deployment notes. |
| `/data` | `/mnt/user/appdata/covalent/data` | read/write | Encrypted chunks, metadata, identity, keys, and local API token. |
| `/source` | One selected `/mnt/user/<share>` | read-only | A chosen share to back up. Add distinct mappings for more shares; never map all of `/mnt/user`. |
| `/boot-source` | `/boot` | read-only | Optional boot-drive backup only. |
| `/restore` | A chosen empty destination share | read/write | Explicit restore destination, not a source or broad root. |

Never map `/mnt/user` or `/boot` writable. `/boot-source` is deliberately separate so boot backup is opt-in and read-only. A restore begins with a signed preview against the exact selected target and chosen conflict policy; traversal, absolute paths, and symlink escapes are rejected by the core before writes.

## Network and first use

Bridge mode maps TLS management on TCP 8443 and authenticated QUIC on UDP 8787. The daemon's cleartext API listens only on loopback inside the container; a pinned Caddy build terminates HTTPS in the same network namespace. LAN discovery defaults off. Set `COVALENT_HTTPS_HOST` to the exact private DNS name clients use, such as `tower.local`, before first enrollment.

First start creates a durable local CA at `/config/caddy/data/caddy/pki/authorities/local/root.crt`. Copy that public certificate over the local Unraid administrative channel, verify it against the file on the host, and enroll that exact CA in Covalent's native setup or the operating-system trust store. Do not accept an arbitrary browser warning, use a trust-all client, or disable hostname checks. A system-trusted same-host reverse proxy or Tailscale Serve is also supported when it forwards only to the loopback management socket.

After trust enrollment, open the HTTPS WebUI and obtain the local token from `/data/local-api-token` in the container console. The web console keeps the token only in its active tab. Pair storage nodes, enter only the provider IDs you mean to use, and choose source mappings one at a time. Covalent never automatically chooses replica placement.

## Upgrade and recovery

Stop the container before backing up or migrating `/data`; keep `/config` and `/data` together because `/config` contains the TLS CA and `/data` contains node identity and encrypted state. On upgrade, retain both mappings and the same UID/GID. To test recovery, restore the two appdata directories to a separate host location, start a new container with those mounts, and verify the HTTPS CA, node identity, and a backup before using it. Do not overwrite an existing production `/data` directory during a restore drill.

The template pins the semantic image version. A release owner must replace it with the published digest after the scan/sign/attest workflow succeeds. Until that public digest exists and the live clean-install/upgrade/share/boot/restore drill passes on a provisioned Unraid host, the template is a validated candidate rather than release evidence.
