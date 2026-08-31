# Docker

For the shortest end-to-end route, start with
[Back up your first folder](../../docs/getting-started.md). This page contains
the Docker-specific detail and recovery contract.

## Personal use from this checkout

This path works before a public `v0.2.0` image exists. It builds the exact
checked-out source, starts in the background, claims the server, and leaves the
source read-only.

Run from the Covalent repository root on Linux or macOS with Docker Desktop.
The example keeps every bind mount below `$HOME`, which Docker Desktop shares by
default on macOS. It uses `sudo` only to create fixed-UID host directories;
Docker itself stays rootless. Replace the source only after this small recovery
check works.

```sh
test -f packaging/docker/Dockerfile
./scripts/setup-doctor.sh docker
covalent_host_root="$HOME/.covalent-server"
sudo install -d -o 65532 -g 65532 -m 700 \
  "$covalent_host_root/config" \
  "$covalent_host_root/data" \
  "$covalent_host_root/secrets" \
  "$covalent_host_root/restore"
# Create one recognizable read-only test file for the first restore check.
sudo install -d -o 65532 -g 65532 -m 500 "$covalent_host_root/source"
printf '%s\n' 'Covalent first-backup check' | \
  sudo tee "$covalent_host_root/source/first-backup.txt" >/dev/null
sudo chown 65532:65532 "$covalent_host_root/source/first-backup.txt"
sudo chmod 400 "$covalent_host_root/source/first-backup.txt"
docker build -f packaging/docker/Dockerfile -t covalent:local .
docker run --rm --user 65532:65532 \
  -v "$covalent_host_root/secrets:/secrets:rw" \
  covalent:local \
  provision-key --key-file /secrets/covalent-kek
export COVALENT_KEK_FILE="$covalent_host_root/secrets/covalent-kek"
# Give Compose these explicit host paths; choose another shared disk or
# filesystem only if needed, and keep each directory owned by the container UID/GID.
export COVALENT_CONFIG_HOST_DIR="$covalent_host_root/config"
export COVALENT_DATA_HOST_DIR="$covalent_host_root/data"
export COVALENT_BACKUP_SOURCE="$covalent_host_root/source"
export COVALENT_RESTORE_TARGET="$covalent_host_root/restore"
sudo ./scripts/validate-setup-paths.sh \
  --config "$COVALENT_CONFIG_HOST_DIR" \
  --data "$COVALENT_DATA_HOST_DIR" \
  --source "$COVALENT_BACKUP_SOURCE" \
  --restore "$COVALENT_RESTORE_TARGET" \
  --kek "$COVALENT_KEK_FILE"
docker compose -f packaging/docker/compose.yaml up -d --no-build
docker compose -f packaging/docker/compose.yaml logs --tail=100 node
```

The path validator is read-only. This example runs it with `sudo` because the
fixed container UID owns the mode-`0700` host directories; without elevation,
the operator account cannot canonicalize them. It rejects broad roots and any
equal, parent, or child overlap between source, restore, configuration,
encrypted data, and the KEK. Do not bypass it. If your real source has a
different path, grant UID `65532` read and traverse access without making that
source writable by the container.

The `provision-key` command is deliberate one-time provisioning, not startup
behavior. The KEK is mounted read-only as `/run/secrets/covalent-kek`, outside
`/config`, `/data`, and every backup source; do not include it in a selected
source. Copying `/config` and `/data` without this file stays locked. Keep the
file mode `0600` and `COVALENT_KEY_ENCRYPTION_KEY_VERSION=1` for the lifetime of
this state directory: the current v0.2.0 contract has no automatic rotation.
The image never generates a missing KEK.

The published v0.1.0 immutable GHCR digest predates this KEK contract and does not contain `provision-key`; it is not an installable release for this workflow. Do not substitute it for `covalent:local`. Production installation stays blocked until a newly signed immutable digest containing this code is published and the Unraid template is updated atomically.

The image is rootless (`65532:65532`), has a read-only root filesystem, drops every Linux capability, enables `no-new-privileges`, and uses a `noexec,nosuid` temporary filesystem. `/data` is the durable encrypted engine state, identity, keys, and local API token. `/config` is sensitive Caddy state: it contains the local CA certificate and its signing key. Back it up with `/data`, but never treat the directory itself as a settings export or a source share.

Compose deliberately fixes the runtime UID/GID at `65532:65532`: Docker Compose mounts file-backed secrets with host ownership, so a configurable `PUID` would make a `0400` KEK unreadable or tempt an unsafe permission change. Create `/config`, `/data`, and the KEK as `65532:65532`; the key file stays `0600` on the host and appears as `0400` in the container. `PUID` and `PGID` overrides are explicitly rejected at startup. `UMASK` accepts a three-digit octal value and defaults to `027`. For a different identity, use an explicit `docker run --user UID:GID` provisioning and runtime contract with a separately owner-readable KEK; do not override the supplied Compose service.

`COVALENT_BACKUP_SOURCE` mounts one selected directory at `/source` read-only. Set it to a specific share, never `/mnt/user`. `COVALENT_RESTORE_TARGET` is the only writable example bind mount and appears at `/restore`. Preview first, choose a conflict policy, then explicitly authorize the signed plan in the console or API.

## Console and local API

A same-container Caddy proxy serves `https://localhost:8443` and can reach the
daemon only over its loopback socket. Claim and enroll the CA before opening
that address; never click through the browser's certificate warning. The
responsive no-framework console implements Pair, Backup, Restore, and Settings
against the daemon's real `/api/v1/*` routes. Status is public; changes require
a token.

Claim a fresh server from a trusted release-channel computer. Read its one-time setup code from the container log, save the code in an owner-only file, then run:

```sh
cargo build --locked --release -p covalent-cli
covalent_cli=./target/release/covalent
claim_parent="$HOME/.config/covalent"
setup_code_file="$claim_parent/docker-setup-code"
claim_output="$claim_parent/docker-claim"
install -d -m 700 "$claim_parent"
install -m 600 /dev/null "$setup_code_file"
# Paste the setup code into setup_code_file with a local editor. Keep its final newline.
test ! -e "$claim_output" # covalent claim must create this new 0700 directory
"$covalent_cli" claim \
  --https-url https://localhost:8443 \
  --setup-code-file "$setup_code_file" \
  --output-dir "$claim_output"
```

The CLI durably saves an owner-only nonce-and-proof request beside the output path before connecting; that pending record contains neither the setup code nor token. It sends the proof rather than the code, decrypts the returned token only if it matches the delivered CA, then verifies that CA, the exact hostname, and the token over a second authenticated HTTPS request. Only then does it create `root.crt` and `local-api-token` mode `0600`, sync them, and remove the pending request. If the command or connection is interrupted, rerun the same command with the same three paths: the CLI reuses the exact request and the server returns the byte-identical sealed response, including after restart. Enroll `root.crt` before opening the console and enter only `local-api-token`; the web console never accepts setup codes.

The first start creates a durable local certificate authority under `/config/caddy/data/caddy/pki/authorities/local/root.crt`. Before entering the token, enroll that exact CA on each native client or operating system and set `COVALENT_HTTPS_HOST` to the DNS name clients use. Never use a trust-all client or bypass hostname verification. Compose publishes HTTPS on host loopback by default; remove `127.0.0.1:` only after the CA is enrolled and the hostname resolves on the intended private network.

### Enroll or remove the claimed CA

For a claimed Covalent HTTPS console, enroll and later remove the exact claimed
CA on the operator computer. This Docker example uses the local
`https://localhost:8443` claim output. On macOS, use the login Keychain:

```sh
claim_output="$HOME/.config/covalent/docker-claim"
ca="$claim_output/root.crt"
test -f "$ca"
ca_sha1=$(openssl x509 -in "$ca" -noout -fingerprint -sha1 | sed 's/^.*=//;s/://g')
test "${#ca_sha1}" = 40
security add-trusted-cert -r trustRoot \
  -k "$HOME/Library/Keychains/login.keychain-db" "$ca"
# Later, remove only this exact certificate:
# security delete-certificate -Z "$ca_sha1" "$HOME/Library/Keychains/login.keychain-db"
```

On Debian or Ubuntu, use the system trust store, then restart the browser:

```sh
claim_output="$HOME/.config/covalent/docker-claim"
sudo install -m 644 "$claim_output/root.crt" \
  /usr/local/share/ca-certificates/covalent-local.crt
sudo update-ca-certificates
# Later:
# sudo rm /usr/local/share/ca-certificates/covalent-local.crt
# sudo update-ca-certificates
```

Other Linux distributions use different CA stores; follow that distribution's
documented local-root procedure. Never use a browser certificate bypass.

The console keeps the token in page memory only. Cleartext bearer-authenticated requests are rejected outside loopback. Do not put the token in shell history. The API/CLI **settings export** is a logical safe export that excludes identity keys, backup keys, grants, provider credentials, and Caddy state; it is not a copy of `/config`.

## First backup and restore check

After the claimed CA is trusted and the console is unlocked:

1. Open **Backup** and keep the source as `/source`.
2. Enter a backup name, keep the suggested snapshot ID, and leave every backup
   device clear for the first local-only test.
3. Choose **Start backup** once. Wait for **Backup complete** and receipt
   confirmation.
4. Open **Restore**, choose that backup and snapshot, and keep the destination
   `/restore` with **Stop on conflicts**.
5. Review and authorize the signed preview, restore, then compare the restored
   file with the source.

This local-only test proves setup, not source-loss protection. Pair another
device, explicitly enable it as a backup device, select it on a later backup,
and Verify from Android or macOS. The complete success checklist is in
[Back up your first folder](../../docs/getting-started.md#you-are-protected-when).

## LAN and Tailscale

LAN discovery defaults to `false`. It is multicast discovery on one local network; it does not discover Tailnet devices. The Compose default keeps TCP 8443 on host loopback and publishes authenticated QUIC UDP 8787 on all host interfaces.

To connect an Android phone on an ordinary trusted LAN, first reserve a DNS name
that resolves to this host from both the host and phone. Then restart with the
host's exact LAN IPv4 address and that certificate name:

```sh
export COVALENT_HTTPS_BIND_IP=192.168.1.50
export COVALENT_PEER_BIND_IP=192.168.1.50
export COVALENT_HTTPS_HOST=covalent.home.arpa
export COVALENT_ADVERTISED_PEER_ADDRESS=192.168.1.50:8787
docker compose -f packaging/docker/compose.yaml up -d --no-build
```

Replace both examples. Confirm `covalent.home.arpa` resolves to this host on the
phone before claiming `https://covalent.home.arpa:8443`; the claim output is
bound to that exact CA and hostname. Permit TCP 8443 and UDP 8787 only from the
intended private LAN devices.

For a Tailnet-only host, bind both published ports to that host's numeric
Tailscale IPv4 address. Keep the HTTPS certificate name as MagicDNS. Leave the
advertised peer override unset when the container can resolve that name, or set
it to a numeric `IP:port`; the node's `SocketAddr` contract does not accept a
hostname there:

```sh
export COVALENT_HTTPS_BIND_IP=100.64.0.10
export COVALENT_PEER_BIND_IP=100.64.0.10
export COVALENT_HTTPS_HOST=atlas.example-tailnet.ts.net
export COVALENT_ADVERTISED_PEER_ADDRESS=100.64.0.10:8787
docker compose -f packaging/docker/compose.yaml up -d --build
```

Confirm `100.64.0.10` is this host's current `tailscale ip -4` result before
using the example. In the Tailnet policy, grant only intended devices TCP 8443
and UDP 8787 to this host; do not use an allow-all rule. Current Tailscale
policy uses a grant such as `"ip": ["tcp:8443", "udp:8787"]` with your own
restricted `src` and Atlas `dst` selectors. The stock container does not run
`tailscaled` or mount its LocalAPI socket, so it does not enumerate Tailnet
peers; Tailscale supplies routing and Covalent still requires confirmed pairing
and a transport certificate pin. See the
[Atlas/Tailscale runbook](../../docs/platform/atlas-tailscale.md) for the full
CA, access-policy, and reachability sequence.

## Multi-architecture, reproducibility, and supply chain

The image supports exactly `linux/amd64` and `linux/arm64`. It uses a pinned Rust Alpine builder, a throwaway pinned Go Alpine stage that compiles Caddy from source, and a pinned Alpine 3.23 runtime base. The OpenSSL libraries are upgraded to the exact signed Alpine security revision `3.5.8-r0`, published after the base image was assembled; exact package pins make repository drift fail the build instead of selecting an unreviewed version. The OCI labels record the Covalent release version, runtime base name and digest, and OpenSSL security revision so CI can reject version, documentation, or image-metadata drift. The release index repeats the exact version as `org.opencontainers.image.version`; promotion refuses to move `latest` to an older version or to an equal version with a different digest.

Caddy is compiled rather than copied out of `caddy:2.11.4-alpine`, because that published binary is linked against `go1.26.3` and no official patched Caddy tag exists yet. `packaging/docker/caddy` is the same three-line consumer module `xcaddy` generates — it imports Caddy's standard module set and adds nothing — and pins upstream snapshot `v2.11.5-0.20260711231708-b2693fb63a30` (commit `b2693fb6`), which adapts Caddy's CEL matcher to the post-v0.28 API. This snapshot is 33 commits after v2.11.4; it is not relabeled as v2.11.4. The consumer module selects patched `cel-go` `v0.30.0` for GO-2026-6094, OpenTelemetry `v1.44.0` for GO-2026-5158, `go-chi` `v5.3.0` for the RealIP advisories, and `klauspost/compress` `v1.18.7` for GO-2026-5841. The full intervening delta and review evidence are recorded in `docs/security/container-image-vulnerabilities.md`. The unchanged `Caddyfile` validates against the resulting binary. The Caddy source version, the whole module graph (`go.mod` + `go.sum`, built `-mod=readonly`) and the toolchain image digest are all pinned, and `GOTOOLCHAIN=local` keeps the build from reaching the network for a different compiler.

Create a local two-architecture OCI archive with Buildx:

```sh
docker buildx build --platform linux/amd64,linux/arm64 \
  -f packaging/docker/Dockerfile -t ghcr.io/thekozugroup/covalent:local \
  --output type=oci,dest=covalent-local.oci .
```

The release workflow builds the same pinned Rust toolchain and Alpine runtime independently for `linux/amd64` and `linux/arm64`, enforces the 96 MiB budget on both images, then assembles one manifest. It scans each private architecture image, emits a distinct SPDX SBOM for each, and keylessly signs the release index plus both child image digests with Cosign OIDC. Each SBOM is attested only to its matching child digest. Verification commands are recorded with the release artifact; a signature is not implied for local developer tags.

Run deterministic container checks locally:

```sh
docker build -f packaging/docker/Dockerfile -t covalent:e2e .
./scripts/check-container-contract.sh covalent:e2e
./scripts/check-container-runtime.sh covalent:e2e
./scripts/docker-compose-e2e.sh covalent:e2e
```

The Compose drill starts three rootless/read-only nodes, confirms pairing, selects two providers explicitly, backs up nested paths, loses the source, rejects local ciphertext corruption, repairs it from paired providers, imports safe settings, and restores only relative paths under `/restore`.

On macOS, include the native Apple trust/enrollment probe in the same packaged-Caddy drill:

```sh
COVALENT_RUN_APPLE_TLS_E2E=true ./scripts/docker-compose-e2e.sh covalent:e2e
```

That probe first confirms the package certificate is rejected by default trust, then verifies the correct enrolled CA and bearer token, rejects a different package CA, rejects a wrong token, and leaves DNS/IP hostname verification enabled.
