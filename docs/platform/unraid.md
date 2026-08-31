# Unraid operation

Start with [Back up your first folder](../getting-started.md), then return here
for Unraid-specific paths and recovery rules.

Unraid is Tier 1. The template is intentionally unprivileged (`99:100`),
read-only, capability-free, and uses `no-new-privileges` with a temporary
filesystem. It is not listed in Community Applications yet. Until a new
immutable `v0.2.0` image is published, do not import or start the historical
template. Do not enable privileged mode to solve mount permissions; correct the
selected host paths instead.

The v0.1.0 template uses the released immutable GHCR digest, not a mutable tag. Docker accepts `image@sha256:…` references and the repository validator rejects any other image form. This prevents a later tag rewrite from changing an existing Unraid install. That digest predates the mandatory KEK and trusted claim client, so **do not install it**: deployment is blocked until a newly signed immutable digest includes those features and updates the template atomically.

## Setting up

These steps are ready for the future `v0.2.0` immutable digest. Today they stop
before template installation because the only public digest is the blocked
historical `v0.1.0` image.

1. **Provision and escrow the KEK before installing or applying a template.**
   The reserved path is `/mnt/user/system/covalent-secrets`. It is never a
   Covalent source. Do not use `/boot`, appdata, `/mnt/user/system`, or any
   enclosing directory as a source.

   For a source-checkout evaluation, build the current image first, then create
   the required key on the Unraid terminal:

   ```sh
   cd /mnt/user/Source/Covalent
   docker build -f packaging/docker/Dockerfile -t covalent:local .
   KEK_DIR=/mnt/user/system/covalent-secrets
   install -d -o 99 -g 100 -m 700 "$KEK_DIR"
   docker run --rm --user 99:100 \
     -v "$KEK_DIR:/secrets:rw" \
     covalent:local \
     provision-key --key-file /secrets/key-encryption-key
   ```

   The owner-only file is mode `0600`. Keep it and version `1` unchanged. Make
   and verify one byte-for-byte offline escrow copy on encrypted removable media
   that is never mounted into Covalent. Exclude the live KEK from every host and
   Covalent backup.
2. After `v0.2.0` publishes, confirm this page and
   `packaging/unraid/covalent.xml` name that exact signed immutable digest. Only
   then import the template manually. Community Applications availability is
   future work and must not be assumed.
3. Set **HTTPS hostname** to the exact name clients use, such as `tower.local`.
4. Map **Configuration** to `/mnt/user/appdata/covalent/config` and **Encrypted
   storage** to `/mnt/user/appdata/covalent/data`. Create the writable paths
   before applying the template:

   ```sh
   install -d -o 99 -g 100 -m 700 \
     /mnt/user/appdata/covalent/config \
     /mnt/user/appdata/covalent/data \
     /mnt/user/Restore/Covalent
   ```
5. Map the KEK file read-only as **KEK secret (reserved system share)**.
6. Map **Selected backup source** to one explicit share, read-only, and add a
   distinct writable restore share such as `/mnt/user/Restore/Covalent`.
   Never select `/mnt/user/system`, its parent, or any path enclosing the KEK.
7. From the same exact source checkout, validate the host paths before startup:

   ```sh
   ./scripts/validate-setup-paths.sh \
     --config /mnt/user/appdata/covalent/config \
     --data /mnt/user/appdata/covalent/data \
     --source /mnt/user/Photos \
     --restore /mnt/user/Restore/Covalent \
     --kek /mnt/user/system/covalent-secrets/key-encryption-key
   ```

   This read-only check rejects broad roots and every equal, parent, or child
   overlap. Run it again for the exact paths shown by `docker inspect` after the
   container is created.
8. Start the container and read its one-time setup code from the Docker log.
9. On a trusted release-channel Mac or Linux computer, save that code in an
   owner-only file and run:

   ```sh
   https_host=tower.local
   claim_parent="$HOME/.config/covalent"
   setup_code_file="$claim_parent/unraid-setup-code"
   claim_output="$claim_parent/unraid-claim"
   install -d -m 700 "$claim_parent"
   install -m 600 /dev/null "$setup_code_file"
   # Paste the setup code into setup_code_file with a local editor. Keep its final newline.
   test ! -e "$claim_output" # covalent claim must create this new 0700 directory
   covalent claim \
     --https-url "https://${https_host}:8443" \
     --setup-code-file "$setup_code_file" \
     --output-dir "$claim_output"
   ```

   The CLI saves an owner-only pending request beside the output path before it
   connects, proves the code, checks the CA fingerprint, then verifies the exact
   HTTPS hostname and token before creating `root.crt` and `local-api-token`
   mode `0600`. If the command is interrupted, rerun the same command with the
   same three paths. It reuses the exact request and removes its pending record
   only after the credentials are durable and authenticated.
10. Enroll that exact `root.crt`, open the same `https_host` on TCP `8443`, and
    enter only `local-api-token`. The WebUI never accepts a setup code. Never
    use trust-all or bypass hostname verification. Follow the exact
    [macOS or Debian/Ubuntu CA enrollment and removal commands](../../packaging/docker/README.md#enroll-or-remove-the-claimed-ca),
    changing only the claim-output path for this Unraid server.

To connect an Android phone, follow the
[verified APK and onboarding guide](android.md). The phone receives only
`root.crt` and `local-api-token` from the completed CLI claim; never copy or
enter the server setup code into the app.

The Unraid template's WebUI shortcut is necessarily rendered with the host IP.
That shortcut is only a convenience link; it may fail strict certificate
hostname verification. Open the configured HTTPS hostname shown in the template
instead. Do not work around this with a hostname bypass.

## First backup and restore check

In the unlocked console, back up `/source` with no backup device selected and
wait for receipt confirmation. Then restore that snapshot to `/restore` with
**Stop on conflicts**, review the signed preview, and compare the restored test
file. This proves setup only. Pair and explicitly select another device before
relying on Covalent for source-loss protection. See the full
[success checklist](../getting-started.md#you-are-protected-when).

## Required and optional mappings

| Container path | Unraid host path | Mode | Purpose |
| --- | --- | --- | --- |
| `/config` | `/mnt/user/appdata/covalent/config` | read/write | Sensitive Caddy state, including the local CA certificate and signing key. Back it up with appdata; it is not a settings export. |
| `/data` | `/mnt/user/appdata/covalent/data` | read/write | Encrypted chunks, metadata, identity, keys, and the wrapped local API-token record. |
| `/run/secrets/covalent-kek` | `/mnt/user/system/covalent-secrets/key-encryption-key` | read-only | Required KEK in the reserved system-share path. `/mnt/user/system` and any enclosing path are forbidden as sources. Keep independent offline escrow. |
| `/source` | One selected `/mnt/user/<share>` | read-only | A chosen share to back up. Add distinct mappings for more shares; never map all of `/mnt/user`. |
| `/boot-source` | `/boot` | read-only | Optional boot-drive backup only. |
| `/restore` | A chosen writable destination | read/write | Add only while restoring; Covalent inventories existing files and applies the selected conflict policy. |

Restores write only beneath the exact selected target and chosen conflict policy; traversal, absolute paths, and symlink escapes are rejected by the core before writes.

## Pairing with phones and laptops

Bridge mode maps TLS management on TCP 8443 and authenticated QUIC on UDP 8787. The daemon's cleartext API listens only on loopback inside the container; a pinned Caddy build terminates HTTPS in the same network namespace.

Covalent has to tell your other devices which address to dial. On bridge networking — Unraid's default — the only address the container can see is its own, typically `172.17.0.2`, which your phone cannot reach: the peer port is published on the Unraid host, not on the container. Covalent resolves this from the **HTTPS hostname** you already set, which is by definition the name your devices use.

When that cannot be resolved from inside the container, Covalent refuses to advertise an address rather than publishing one that does not work, and says so in the log with the exact remedy. A device that appears in a list and then times out is worse than one that never appears. Fill in **Address other devices dial** with this server's LAN address and peer port, such as `192.168.1.50:8787`, and restart. Pairing routes report `peer_endpoint_unavailable` until an address is known; the console shows this as a sentence, not an error code.

**Find devices on your network** is off by default. Turning it on lets devices find this server by name instead of you typing an address; only the device name, address, and certificate fingerprint are announced, and pairing still has to be confirmed on both devices.

## Upgrade and recovery

Stop the container before backing up or migrating `/data`; keep `/config` and `/data` together because `/config` contains Caddy's local CA and signing key while `/data` contains node identity, encrypted state, and the record that this server has an owner. Retain the separate KEK file and the same version too: appdata alone is intentionally locked. Recovery also requires the independently escrowed KEK; do not recover it from a Covalent backup of the same host. On upgrade, retain all three mappings and the same UID/GID.

Upgrading a deployment that already has a local API token preserves its owner and
does not require setup-code claiming. An installation that already works cannot
acquire a new way to be claimed.

To test recovery, restore the two appdata directories to a separate host location, start a new container with those mounts, and verify the HTTPS CA, node identity, and a backup before using it. Do not overwrite an existing production `/data` directory during a restore drill.

## Release status of this template

Stated plainly, because the previous wording described itself as "a validated candidate rather than release evidence" without saying which parts were unverified.

**Verified by direct execution** against an image built from this repository, running under bridge networking with the template's own security flags (`--user=99:100 --read-only --cap-drop=ALL --security-opt=no-new-privileges`):

- Source-built package coverage exercises the server-side setup-code claim protocol from empty `/config` and `/data` with an explicit separate KEK. The current v0.1.0 released image cannot exercise this flow because it predates the required KEK support.
- The delivered certificate is byte-identical to the one inside the container, and a later connection validating strictly against it succeeds.
- Substituting a different certificate authority makes the sealed reply fail to open.
- A wrong code is refused with `claim_code_incorrect`. The exact original
  nonce-and-proof request replays the same sealed response after a lost response
  or restart; a new request, even from the same code, is refused with
  `claim_unavailable`.
- Restarting a claimed container prints no new code. Only its exact original
  request remains recoverable; every different claim stays closed.
- With an advertised address configured, `transport/identity`, `discovery`, and `pair/invitations` all succeed.
- A backup of a mounted read-only share completes and verifies intact.

**Not yet verified**, and required before this is called production deployment evidence:

- The template has not been installed through Community Applications on a real Unraid host; it was exercised as a plain Docker container with the template's arguments.
- The upgrade, boot-drive, and restore drills have not been run on a provisioned Unraid host.

For a private-network or Tailnet deployment, use the explicit-address and CA steps in the [Atlas/Tailscale runbook](atlas-tailscale.md). Automatic LAN discovery and Tailscale routing are separate features.
