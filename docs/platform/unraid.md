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

Bridge mode is the safe default: map TCP 8787 for the console/local API and UDP 8787 for authenticated QUIC. LAN discovery defaults off. Use manual LAN addresses, a host/sidecar Tailscale address, or host networking if multicast discovery is needed. Host mode expands the console's LAN exposure; protect it with Unraid/firewall controls. Tailscale and discovery only provide reachability, not authorization: every device still needs short-code confirmation and pinned identity pairing.

After the container starts, open its WebUI and obtain the local token from `/data/local-api-token` in the container console. The web console keeps the token only in its active tab. Pair storage nodes, enter only the provider IDs you mean to use, and choose source mappings one at a time. Covalent never automatically chooses replica placement.

## Upgrade and recovery

Stop the container before backing up or migrating `/data`; keep `/config` and `/data` together in your normal Unraid appdata backup policy. On upgrade, retain both mappings and the same UID/GID. To test recovery, restore the two appdata directories to a separate host location, start a new container with those mounts, and verify a backup before using it. Do not overwrite an existing production `/data` directory during a restore drill.
