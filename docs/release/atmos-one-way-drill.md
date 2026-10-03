# Mac–Atmos one-way release drill

This runbook describes the current opt-in, isolated release-candidate drill. It
has not been run against the current candidate. It validates the packaged
transfer engine across a Mac and Atmos; it does not validate the installed Mac
app, LocalNodeManager, Keychain, sandbox inheritance, or saved folder bookmarks.

## Before running

1. Restore normal authenticated access to the `Atmos` Tailnet SSH host. Do not
   substitute Atlas; it remains offline.
2. Reconcile every exact resource in
   `artifacts/validation-2026-09-12/docker-rust-cache-checkpoint48/cleanup-ledger.json`.
   Pass `--prior-cleanup-confirmed` only after that ledger is resolved.
3. Confirm Atmos already has the pinned `moby/buildkit:v0.22.0` image. The drill
   refuses to introduce that shared image. It also checks Docker, Tailscale,
   available memory, unused ports, and its own resource names before creating
   anything.
4. Check out the exact clean release revision. Tracked and untracked changes
   make the drill fail before SSH.
5. Build the personal Apple Silicon package with
   `scripts/build-personal-macos-app.sh`. Unpack its `Covalent.app` into a private
   location and retain the adjacent
   `Covalent-v<VERSION>-macOS-arm64-personal.zip.build-receipt.json`.

The build receipt binds the clean source commit and source fingerprint to the
app executable, node, rclone worker, guardian, and engine manifest. The drill
verifies those component bytes before its first remote action. The archive hash
remains builder evidence; this engine drill does not reverify the archive.

## Run

From the same clean checkout, set `--mac-app` to the verified app you unpacked:

```sh
revision="$(git rev-parse HEAD)"
version="$(scripts/release-version.sh print)"
scripts/test-remote-drill.sh \
  --source-revision "$revision" \
  --release-version "$version" \
  --mac-app "/private/path/Covalent.app" \
  --mac-build-receipt "artifacts/install/Covalent-v${version}-macOS-arm64-personal.zip.build-receipt.json" \
  --prior-cleanup-confirmed \
  --execute
```

`--ssh Atmos` is optional because `Atmos` is the default. Omitting `--execute`
prints usage and makes no local or remote changes.

The drill copies the verified node, rclone worker, and guardian into one
nonce-owned private fixture. It removes their inherited app-sandbox signatures,
ad-hoc signs the copies without entitlements, and updates only the copied engine
manifest hashes. The original app is never changed. Results from these helper
copies are engine and cross-host evidence, not installable-Mac acceptance.

The isolated container uses these explicit test-owned mounts:

- `/sync`: writable destination for the Mac-to-Atmos link.
- `/source`: read-only synthetic application export created with no active
  writer. It is not live appdata and makes no live-database claim.
- `/boot-source`: read-only readable boot-file fixture. It proves file copying,
  not bootability or full boot recovery.

The drill pairs disposable nodes and checks exact bytes, destination-only file
retention, no reverse flow, source immutability, read-only mount enforcement,
and independent Atmos-to-Mac copies from `/source` and `/boot-source`. It never
reads real appdata or a real boot device and never stops existing services.

## Cleanup and failures

The script owns only its nonce-named local directory, remote directory,
container, builder, BuildKit container and volume, and tagged image. It records
the pre-existing container and image IDs and requires them to survive. It does
not prune Docker or alter existing services.

Successful cleanup removes only resources whose nonce ownership is proven and
retains the pre-existing pinned BuildKit image. If process shutdown, Docker
inventory, ownership inspection, removal, or post-cleanup verification is
uncertain, the script returns failure and preserves the owned fixture path for
diagnosis. Record the printed exact resource names and reconcile them before a
later run.
