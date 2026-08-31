# Setup troubleshooting

Start with the sentence that matches what you see. Do not reset state or create
a new claim unless the recovery step explicitly says to do so.

## The server will not start

Check the first error in the container log:

```sh
docker compose -f packaging/docker/compose.yaml logs --tail=100 node
```

- **KEK missing or unreadable:** confirm the configured file exists, is owned
  by the runtime identity, is mode `0600` on the host, and is mounted read-only
  at `/run/secrets/covalent-kek`.
- **Path check failed:** configuration, data, source, restore, and KEK paths
  must be separate. Do not weaken this check; choose a different path.
- **Port already in use:** find the existing TCP `8443` or UDP `8787` listener.
  Stop it or deliberately choose another published port before restarting.
- **PUID/PGID rejected:** the supplied Docker Compose path deliberately uses
  `65532:65532`; the Unraid template deliberately uses `99:100`.

## Claim stopped or lost its response

Run the exact same `covalent claim` command again with the same HTTPS URL,
setup-code file, and output path. The client reuses its durable pending request,
and the server replays only that exact sealed response.

Do not request another code, delete the pending request, or choose a different
output directory. If the code was typed incorrectly, correct the owner-only
setup-code file and rerun before its window expires.

## The browser shows a certificate warning

Stop. Do not bypass the warning.

- Open the exact hostname used when the server certificate was created.
- Enroll the `root.crt` produced by `covalent claim`, not a file copied from the
  server's private configuration directory.
- If the URL uses an IP address but the certificate names a DNS host, use the
  DNS host.

## The access token is rejected

Use `local-api-token` from the successful claim output. Never read a token from
`/data`, container logs, or a setup-code file. Remove surrounding whitespace
introduced by manual copying, then retry over HTTPS with the claimed CA.

## A device cannot find or reach another device

- TCP `8443` is management traffic; UDP `8787` is peer traffic. Permit both
  only between intended LAN or Tailnet devices.
- LAN discovery does not cross subnets or discover Tailnet devices.
- In Docker bridge mode, advertise the host's numeric reachable address, not
  the container's `172.*` address and not a DNS name where a numeric
  `IP:port` is required.
- Confirm the displayed identities, roles, and comparison code on both devices
  before finalizing.

## Backup says the source is unavailable

- Confirm the selected host source exists and the runtime UID can read every
  needed directory component.
- Mount sources read-only.
- Never select a broad root such as `/`, `/srv`, `/mnt`, `/mnt/user`, a system
  share, app data, or a directory containing the KEK.
- Rerun `scripts/validate-setup-paths.sh` with the exact host paths.

## Restore is blocked

- Choose a writable destination separate from the source, configuration, data,
  and KEK.
- Preview first. Read the destination and conflict policy before authorizing.
- For an existing target, choose Stop, Skip, Keep both, or Replace deliberately.
  Replace needs an additional confirmation.
- A symlink or path that escapes the authorized root is rejected by design.

## Android will not install or update

- Install `app-debug.apk`; `app-release-unsigned.apk` is not installable.
- `adb install -r` works only while the APK uses the same debug signing key.
- A build made with another debug key or a future production signer cannot
  update this personal build in place. Uninstalling clears protected connection
  state and folder grants, so export safe settings and record recovery steps
  first.

## macOS blocks the personal-use app

The personal app is ad-hoc signed and intentionally not notarized. Verify its
checksum, architecture, and code signature first. Then Control-click the app
and choose **Open**, or approve that exact app in **Privacy & Security**.

Never disable Gatekeeper globally and do not run a broad recursive `xattr`
command over Downloads or Applications.

## Still stuck

Run the appropriate read-only prerequisite check:

```sh
./scripts/setup-doctor.sh docker
./scripts/setup-doctor.sh macos
./scripts/setup-doctor.sh android
```

For a bug report, include the platform, Covalent version, failing command, and
the non-secret error text. Never attach setup codes, access tokens, KEKs,
private keys, or unredacted device identities.
