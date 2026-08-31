# Back up your first folder

This is the shortest safe path from a new checkout to a backup you have
restored successfully. Allow about 20–30 minutes for the first source build;
later starts take only a few minutes.

## Choose your setup

| What you want | Start here |
| --- | --- |
| Try Covalent on one Apple Silicon Mac | [Personal-use macOS setup](platform/macos.md) |
| Keep an always-on server and use Android or another computer | [Docker setup](../packaging/docker/README.md) |
| Prepare or evaluate Unraid from source; deployment is blocked pending the v0.2.0 image | [Unraid setup](platform/unraid.md) |
| Review Atlas/Tailscale checks; deployment is blocked pending the v0.2.0 image | [Atlas/Tailscale setup](platform/atlas-tailscale.md) |
| Install the personal-use Android APK | [Android setup](platform/android.md) |

The public `v0.1.0` image is historical and must not be installed. Until the
`v0.2.0` release is published, the usable server path is the
[Docker local build](../packaging/docker/README.md#personal-use-from-this-checkout).
Unraid template and Atlas deployment remain blocked; their guides retain
read-only checks and source-evaluation preparation only. The macOS ad-hoc app
and Android debug APK are supported for personal use. Apple Developer ID/notarization is not part
of this setup. Android production signing is deferred.

iOS and Windows are unsupported.

## Before you start

You need:

- one folder containing a few test files;
- a different empty folder for the restore test;
- enough free space for the source, encrypted backup, and restored copy;
- the tools for the path you chose.

From the repository root, check the exact prerequisites:

```sh
./scripts/setup-doctor.sh docker   # Docker or Unraid server work
./scripts/setup-doctor.sh macos    # Apple Silicon personal app
./scripts/setup-doctor.sh android  # Personal-use APK
```

The doctor changes nothing. It prints every missing dependency and the next
safe action.

## 1. Install the server or local app

### Fastest: one Mac

Follow [Personal-use macOS setup](platform/macos.md). The app creates and
protects its own local node automatically. Do not enter an Atlas token or setup
code during normal Mac first run.

### Always-on server

Use one of these paths:

- [Docker from this checkout](../packaging/docker/README.md#personal-use-from-this-checkout) — current usable server path
- [Unraid](platform/unraid.md#setting-up) — deployment blocked; source evaluation and preparation only
- [Atlas through Tailscale](platform/atlas-tailscale.md) — deployment blocked; read-only checks and preparation only

Keep these host locations separate: configuration, encrypted data, selected
source, writable restore target, and the KEK file. The setup guide runs
`scripts/validate-setup-paths.sh` before startup; do not bypass a failed check.

## 2. Claim an always-on server

Skip this section for the normal Mac-only path.

The server log shows one short-lived setup code on its first start. Claim it
once from a trusted Mac or Linux computer with the matching Covalent CLI. The
server guide gives an exact command that:

1. saves the code in a mode-`0600` file;
2. verifies the server's exact private CA and hostname;
3. writes `root.crt` and `local-api-token` to a new owner-only directory;
4. safely resumes the same request if the connection is interrupted.

The setup code is never an Android, macOS, or web-console credential. Those
clients receive only the claimed CA and access token. Keep the original claim
directory on the trusted operator computer.

## 3. Connect a client

- **Android:** follow [Connect in Covalent](platform/android.md#connect-in-covalent).
- **Mac:** its own node is already ready. Use **Devices** to pair it with the
  always-on server or another backup device.
- **Web console:** enroll the claimed `root.crt` in the browser or operating
  system, open the exact HTTPS hostname, and enter `local-api-token`. The token
  stays in that browser tab.

For LAN use, both devices must reach TCP `8443` for HTTPS management and UDP `8787`
for peer traffic. Tailscale is optional; ordinary LAN addresses work.

## 4. Make a small first backup

Start with a disposable folder containing one recognizable file.

1. Open **Backup**. On macOS, choose **New Backup**.
2. Choose the test source. In the Docker web console, use `/source`.
3. Name the backup and keep the suggested snapshot ID.
4. Leave every backup device unselected for this first local-only test.
5. Choose **Start backup** once. Android labels this **Queue backup**.
6. Wait for **Backup complete** in the web console or macOS, or **Completed**
   on Android, and wait for receipt confirmation.

Do not close the browser or app while it is confirming the terminal receipt.
If the connection drops, use the shown retry action; Covalent reuses the same
durable job rather than starting a duplicate.

## 5. Verify it

On Android or macOS, open the completed backup and choose **Verify**. Continue
only when Covalent reports **Verified: everything checked is intact** or the
equivalent intact result.

The web console currently lists completed backups but does not expose the
Verify action. If you used only the web console, complete the restore test below
before treating setup as successful, then verify from a paired Android or Mac
client.

## 6. Restore into a different folder

Never restore this test over the source.

1. Open **Restore** and select the completed backup and snapshot. On macOS,
   choose **Preview Restore…** from the completed backup.
2. Choose the separate empty restore folder. In Docker, use `/restore`.
3. Keep **Stop on conflicts** for the first test.
4. Preview the signed plan. Confirm the displayed destination and file list.
5. Authorize that exact preview, then choose **Restore**. Android labels the
   durable transfer action **Queue restore**.
6. Open the restored file and compare it with the source.

Setup is successful only after the restored copy matches.

## 7. Add real source-loss protection

A local-only backup does not survive loss of that device. Pair at least one
other Covalent device, explicitly enable it as a backup device, confirm its
capacity is current, and select it for the next backup. Run Verify again.

For an always-on server, also escrow its KEK offline. The encrypted data and its
only KEK must never live in the same backup set.

## You are protected when

- the latest backup says **Backup complete**;
- Verify reports the selected copies are intact;
- a restore to a separate folder reproduces the expected files;
- at least one explicitly selected backup device is reachable for source-loss
  protection;
- server configuration, encrypted state, and the independently escrowed KEK
  have a tested recovery plan.

If any step fails, use the [setup troubleshooting guide](troubleshooting.md).
Architecture, protocol, release, and contributor detail stays in the broader
[`docs`](.) tree so it does not slow down first setup.
