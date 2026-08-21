# Unraid operation

Unraid is Tier 1. Install `packaging/unraid/covalent.xml` through Community Applications or Docker templates. It is intentionally unprivileged (`99:100`), read-only, capability-free, and uses `no-new-privileges` with a temporary filesystem. Do not enable privileged mode to solve mount permissions; make the selected host paths writable by the container identity instead.

## Setting up

**Nine steps, none of them in a terminal.** Setup used to take twenty steps, four of which required a container console: reading a bearer token out of `/data/local-api-token`, copying a root certificate out of `/config/caddy/...`, and verifying that certificate by hand. None of that remains.

1. Install the Covalent template from Community Applications.
2. Set **HTTPS hostname** to the name you will type in your browser to reach this server, such as `tower.local`. Covalent's certificate is issued for that name, so it has to match.
3. Map **Configuration** to `/mnt/user/appdata/covalent/config`.
4. Map **Encrypted storage** to `/mnt/user/appdata/covalent/data`.
5. Map **Selected backup source** to one share you want backed up, read-only. Add a separate mapping for each further share.
6. Start the container.
7. Open the container's **Log** from the Unraid Docker page. Covalent prints a setup code in a box near the end:

   ```
     ┌──────────────────────────────────────────────┐
     │  Covalent setup code                         │
     │                                              │
     │      9T96Y-GM7ZJ                             │
     │                                              │
     │  Enter this in the Covalent app or web page  │
     │  to finish setting up this server.           │
     │                                              │
     │  Valid for 30 minutes, and usable once.      │
     │  Restart this container for a new code.      │
     └──────────────────────────────────────────────┘
   ```

8. Open the WebUI.
9. Enter the setup code.

That is the whole flow. There is no token to copy, no certificate file to move, and nothing to verify by hand — entering the code is what establishes trust, and the section below explains why that is safe rather than merely convenient.

If the code expires before you use it, restart the container and read the new one. A code is single-use: once a device has been set up, the container prints no further codes and the setup route refuses every request.

## What the setup code actually does

The code is a credential, and it is worth being precise about what it protects and what it does not.

**The code is never sent over the network.** Your browser or app proves it knows the code by sending a message authentication code computed over a random nonce under a key derived from it. Someone watching the network learns nothing they can reuse.

**The reply is sealed to the code.** Covalent returns the local API token encrypted under a key derived from the same code, with the SHA-256 of the certificate authority bound in as associated data. An attacker sitting between you and the server — the classic risk on a first connection, because your browser does not trust the server's certificate yet — can relay the exchange but ends up holding ciphertext it cannot open. It also cannot substitute its own certificate authority, because changing the certificate breaks the seal. Successfully decrypting the reply is simultaneously proof that the responder held the code and proof that the certificate came from that same responder. That is exactly the check the old instructions asked you to perform by hand with a file comparison.

**Guessing is impractical.** The code carries 50 bits of entropy, and the key derived from it is stretched through 2^18 sequential BLAKE3 compressions. Covalent pays that cost once, at startup, because it knows the code. An attacker pays it for every guess, which puts even an offline search far beyond the thirty minutes the code is alive. Online guessing is separately limited: presentations closer together than 500 ms are refused, and the window closes permanently after 16 incorrect codes.

**Who can read the code.** Anyone who can read this container's log — through the Unraid web interface, the Docker socket, or the host filesystem. All three already require administrative access to the server, and all three could already read `/data/local-api-token` directly, which is precisely what the previous instructions told you to do. The code therefore grants nothing that observing it did not already imply. It is not a new exposure; it removes one.

**If nobody claims it.** The window expires and the server keeps running, unclaimed. The code exists only in memory and is never written to disk, so restarting genuinely mints a new secret rather than redisplaying the old one.

## Required and optional mappings

| Container path | Unraid host path | Mode | Purpose |
| --- | --- | --- | --- |
| `/config` | `/mnt/user/appdata/covalent/config` | read/write | Operator-owned exported safe settings and the TLS certificate authority. |
| `/data` | `/mnt/user/appdata/covalent/data` | read/write | Encrypted chunks, metadata, identity, keys, and the local API token. |
| `/source` | One selected `/mnt/user/<share>` | read-only | A chosen share to back up. Add distinct mappings for more shares; never map all of `/mnt/user`. |
| `/boot-source` | `/boot` | read-only | Optional boot-drive backup only. |
| `/restore` | A chosen empty destination | read/write | Add only while restoring. |

Restores write only beneath the exact selected target and chosen conflict policy; traversal, absolute paths, and symlink escapes are rejected by the core before writes.

## Pairing with phones and laptops

Bridge mode maps TLS management on TCP 8443 and authenticated QUIC on UDP 8787. The daemon's cleartext API listens only on loopback inside the container; a pinned Caddy build terminates HTTPS in the same network namespace.

Covalent has to tell your other devices which address to dial. On bridge networking — Unraid's default — the only address the container can see is its own, typically `172.17.0.2`, which your phone cannot reach: the peer port is published on the Unraid host, not on the container. Covalent resolves this from the **HTTPS hostname** you already set, which is by definition the name your devices use.

When that cannot be resolved from inside the container, Covalent refuses to advertise an address rather than publishing one that does not work, and says so in the log with the exact remedy. A device that appears in a list and then times out is worse than one that never appears. Fill in **Address other devices dial** with this server's LAN address and peer port, such as `192.168.1.50:8787`, and restart. Pairing routes report `peer_endpoint_unavailable` until an address is known; the console shows this as a sentence, not an error code.

**Find devices on your network** is off by default. Turning it on lets devices find this server by name instead of you typing an address; only the device name, address, and certificate fingerprint are announced, and pairing still has to be confirmed on both devices.

## Upgrade and recovery

Stop the container before backing up or migrating `/data`; keep `/config` and `/data` together because `/config` contains the TLS CA and `/data` contains node identity, encrypted state, and the record that this server has an owner. On upgrade, retain both mappings and the same UID/GID.

Upgrading a deployment that was set up before setup codes existed is a no-op: the node sees an existing local API token, records that it already has an owner, and never offers a code. An installation that already works cannot acquire a new way to be claimed.

To test recovery, restore the two appdata directories to a separate host location, start a new container with those mounts, and verify the HTTPS CA, node identity, and a backup before using it. Do not overwrite an existing production `/data` directory during a restore drill.

## Release status of this template

Stated plainly, because the previous wording described itself as "a validated candidate rather than release evidence" without saying which parts were unverified.

**Verified by direct execution** against an image built from this repository, running under bridge networking with the template's own security flags (`--user=99:100 --read-only --cap-drop=ALL --security-opt=no-new-privileges`):

- The container starts from empty `/config` and `/data` and prints a setup code to stdout.
- The setup code is accepted once and returns a working API token and the CA certificate.
- The delivered certificate is byte-identical to the one inside the container, and a later connection validating strictly against it succeeds.
- Substituting a different certificate authority makes the sealed reply fail to open.
- A wrong code is refused with `claim_code_incorrect`; a replayed code is refused with `claim_unavailable`.
- Restarting a claimed container prints no new code and the route stays closed.
- With an advertised address configured, `transport/identity`, `discovery`, and `pair/invitations` all succeed.
- A backup of a mounted read-only share completes and verifies intact.

**Not yet verified**, and required before this is called release evidence:

- The template has not been installed through Community Applications on a real Unraid host; it was exercised as a plain Docker container with the template's arguments.
- `<Repository>` still names a semantic tag rather than a published digest. The release owner must replace it with the digest after the scan/sign/attest workflow succeeds, so the template cannot silently drift to a different image.
- The upgrade, boot-drive, and restore drills have not been run on a provisioned Unraid host.
