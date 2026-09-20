# Atlas / Tailscale install runbook

Start with [Create your first one-way link](../getting-started-links.md). This
runbook adds the Unraid, SSH, and Tailnet checks needed for Atlas.

Atlas is offline; no Atlas runtime validation is claimed. Release installation
remains conditional on publication of the verified v0.2.1 immutable image and
matching template. The historical v0.1.0 image predates the required KEK and
trusted claim client and must not be installed.

## 1. Historical v0.1.0 boundary (do not install)

The following digest identifies the historical v0.1.0 release only:

```text
ghcr.io/thekozugroup/covalent@sha256:8b8b96bdea7437fecf6d9c3297c248fd9de7eeb25fe7d701aa6f0a5b633cf8a6
```

It contains `linux/amd64` and `linux/arm64` manifests, but it must not be used
for any install, KEK, or claim step. Verify it only to establish the historical
release boundary that the next immutable digest must replace:

```sh
cosign verify ghcr.io/thekozugroup/covalent@sha256:8b8b96bdea7437fecf6d9c3297c248fd9de7eeb25fe7d701aa6f0a5b633cf8a6 \
  --certificate-identity 'https://github.com/thekozugroup/Covalent/.github/workflows/container-supply-chain.yml@refs/tags/v0.1.0' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

### Verify the replacement v0.2.1 image when published

Download `covalent-container-digest.txt` and the matching Unraid template from
the published v0.2.1 release. No replacement digest is asserted here before the
release lane finishes. The template must contain exactly that immutable digest.
Verify it before pulling or installing:

```sh
version=v0.2.1
digest=$(cat covalent-container-digest.txt)
printf '%s\n' "$digest" | grep -Eq '^sha256:[0-9a-f]{64}$' || exit 1
case "$version" in
  v0.2.1) signing_ref=release-tools-v0.2.1-container ;;
  *) signing_ref="$version" ;;
esac
cosign verify "ghcr.io/thekozugroup/covalent@$digest" \
  --certificate-identity "https://github.com/thekozugroup/Covalent/.github/workflows/container-supply-chain.yml@refs/tags/${signing_ref}" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

For v0.2.1 alone, the exact signing identity is
`https://github.com/thekozugroup/Covalent/.github/workflows/container-supply-chain.yml@refs/tags/release-tools-v0.2.1-container`.
The packaging repair must reuse the original scanned v0.2.1 architecture
archives and retain their source provenance. The runtime source remains the
signed `v0.2.1` tag. The separate repair tag identifies signing/publication
code, not a different application version. Preserve the exact identity check;
do not substitute a branch, wildcard or different repair tag. Verify the
architecture signatures and SBOM attestations using the release's matching
verification instructions before deployment.

## 2. Keep the container boundary small and the KEK separate

Use the template's rootless, read-only, no-capability settings. Persist only
`/config` and `/data`. Map each backup source separately and read-only. Add a
writable `/restore` mapping only for a specific restore. Never map all of
`/mnt/user`, the Docker socket, or the host Tailscale state directory.

Before installing a release, create the KEK once in the exact
reserved Unraid system-share directory used by the template, then mount that
one file read-only at `/run/secrets/covalent-kek`. Do not pass this directory,
its `system` share, or any enclosing path as a `--source`; never mount it at
`/source` or `/boot-source`. For a current source-checkout evaluation, build
the local image on the Atlas Unraid terminal, then run:

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

The Unraid template maps
`/mnt/user/system/covalent-secrets/key-encryption-key` read-only and runs as its
fixed `99:100` identity. The file must remain owner-only (`0600`) and owned by
that identity. Do not substitute the generic Docker Compose UID, path, or
secret filename on Atlas. The version must remain `1` for the state directory.
There is no automatic KEK rotation in v0.2.1. A copied
`/config` + `/data` pair without this separate secret fails locked; Covalent
never creates a replacement key during startup.

Set `COVALENT_HTTPS_HOST` to the exact name clients will use. Publish TCP 8443
only where you intend to use the HTTPS console and UDP 8787 only where peers must
reach QUIC. Do not publish the daemon's loopback HTTP port.

## 3. Claim over HTTPS, then enroll the exact CA

On first start, Caddy writes the exact local CA and its sensitive signing state under:

```text
/config/caddy/data/caddy/pki/authorities/local/root.crt
```

After the replacement release is published and started, read the one-time setup code from the server log. On a trusted
release-channel Mac or Linux computer, install the matching verified CLI archive
using the [CLI install guide](../release/cli-install.md). This avoids a source
build and never uses a curl-pipe-shell installer. Save the setup code in an
owner-only file and run:

```sh
claim_parent="$HOME/.config/covalent"
setup_code_file="$claim_parent/atlas-setup-code"
claim_output="$claim_parent/atlas-claim"
install -d -m 700 "$claim_parent"
install -m 600 /dev/null "$setup_code_file"
# Paste the setup code into setup_code_file with a local editor. Keep its final newline.
test ! -e "$claim_output" # covalent claim must create this new 0700 directory
covalent claim \
  --https-url https://atlas.example-tailnet.ts.net:8443 \
  --setup-code-file "$setup_code_file" \
  --output-dir "$claim_output"
```

`covalent claim` first saves an owner-only pending nonce and proof beside the
output path; it never stores the setup code or token there. It sends the proof
rather than the code, checks the returned CA's SHA-256 fingerprint, decrypts the
sealed token, then performs an authenticated HTTPS request using that exact CA
and the exact hostname. It creates a new private output directory only after all
checks pass, containing `root.crt` and `local-api-token` mode `0600`, and removes
the pending record only after those credentials are durable. If the command or
connection is interrupted, rerun the same command with the same URL, setup-code
file, and output path; the client reuses the exact request and the server replays
the exact sealed response, including across a server restart. Enroll `root.crt`
in the client or browser trust store, preserve hostname verification, then open
`https://atlas.example-tailnet.ts.net:8443` and enter only `local-api-token`. The web
console is token-only; it never accepts a setup code.

Use the exact
[macOS or Debian/Ubuntu CA enrollment and removal commands](../../packaging/docker/README.md#enroll-or-remove-the-claimed-ca).
Replace only the claim-output path; keep Atlas's exact MagicDNS hostname. Never
copy Caddy's private CA signing state out of `/config`.

Android also never accepts the setup code. Claim once with the verified CLI,
then use only this output directory's `root.crt` and `local-api-token` to enroll
the phone as described in the [Android install and onboarding guide](android.md).

Do not use trust-all or a hostname bypass. The CLI presently accepts DNS
hostnames (Atlas MagicDNS), not bracketed IPv6 URL literals.

## 4. Choose discovery or routing deliberately

Automatic LAN discovery is opt-in multicast on the local network. It is not a
Tailnet directory and should remain off when manual reachability is preferred.

Tailscale provides routing, not Covalent identity or discovery. For Atlas, pair
with the explicit MagicDNS endpoint **`atlas.example-tailnet.ts.net:8787`**.
Leave `COVALENT_ADVERTISED_PEER_ADDRESS` unset first: the container entrypoint
resolves `COVALENT_HTTPS_HOST` to a concrete IPv4 `SocketAddr`. If that
resolution cannot produce the Tailnet address, set the override to the host's
numeric Tailscale IP and peer port, for example `100.64.0.10:8787`. Never put a
MagicDNS hostname in this variable; the node CLI accepts only numeric
`IP:port` (or bracketed numeric IPv6 plus port) syntax.

Restrict Tailnet reachability before publishing the ports. This current
Tailscale grant fragment is an example; replace the user and tag ownership with
your own policy, assign `tag:covalent-atlas` to Atlas, and keep the operator SSH
group narrower than the client group:

```json
{
  "groups": {
    "group:covalent-clients": ["owner@example.com"],
    "group:covalent-operators": ["owner@example.com"]
  },
  "tagOwners": {
    "tag:covalent-atlas": ["autogroup:admin"]
  },
  "grants": [
    {
      "src": ["group:covalent-clients"],
      "dst": ["tag:covalent-atlas"],
      "ip": ["tcp:8443", "udp:8787", "tcp:8789"]
    },
    {
      "src": ["group:covalent-operators"],
      "dst": ["tag:covalent-atlas"],
      "ip": ["tcp:22"]
    }
  ]
}
```

Review and test this fragment in the Tailscale policy editor; grants are
deny-by-default additions, so an existing broader rule can still grant more
access. The authoritative syntax is the
[Tailscale grants reference](https://tailscale.com/docs/reference/syntax/grants).

The stock image does not run `tailscaled` and does not receive a Tailscale
LocalAPI socket. Therefore it cannot list Tailnet peers itself. Do not mount the
host socket or state directory just to gain discovery; use the explicit address
flow. Pairing still compares identities and a confirmation code, then pins the
peer transport certificate.

## 5. Run the read-only preflight

The operator computer needs:

- this exact Covalent source checkout, because the preflight and template
  validator run from it;
- `ssh`, `ssh-keygen`, `ssh-keyscan`, and a working key-only SSH login;
- an Atlas SSH account written as `USER@atlas.example-tailnet.ts.net` that logs in as UID 0 (normally Unraid's `root` account); the preflight refuses a weaker identity check;
- Atlas's ED25519 SSH fingerprint obtained from its local console.

`BatchMode=yes` deliberately rejects password prompts. Set up and test the SSH
key before running Covalent's preflight. Atlas itself needs `docker`,
`tailscale`, `jq`, `python3`, `setpriv`, `ss`, `find`, `getent`, and GNU `readlink -e` or
`realpath`; the helper fails closed when an identity or DNS-verification tool is
unavailable.

Run the read-only Atlas preflight before any host-side install or deployment.
First pin the SSH host key. On Atlas's local console, obtain the trusted
ED25519 fingerprint out of band:

```sh
ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
```

Then, on the operator computer, scan the key, compare the printed SHA-256
fingerprint exactly with the local-console value, and append it only after it
matches:

```sh
install -d -m 700 "$HOME/.ssh"
candidate_key=$(mktemp)
trap 'rm -f "$candidate_key"' 0 1 2 3 15
ssh-keyscan -H -t ed25519 atlas.example-tailnet.ts.net > "$candidate_key"
ssh-keygen -lf "$candidate_key"
printf 'Atlas console ED25519 SHA256 fingerprint: ' >&2
IFS= read -r trusted_fingerprint
scanned_fingerprint=$(ssh-keygen -lf "$candidate_key" | awk '{print $2}')
if [ "$scanned_fingerprint" != "$trusted_fingerprint" ]; then
  echo 'Atlas SSH host-key fingerprint mismatch; refusing to trust it' >&2
  exit 1
fi
cat "$candidate_key" >> "$HOME/.ssh/known_hosts"
chmod 600 "$HOME/.ssh/known_hosts"
ssh -o BatchMode=yes -o StrictHostKeyChecking=yes \
  root@atlas.example-tailnet.ts.net true
```

Do not trust the `ssh-keyscan` result by itself, and do not delete a changed
known-host entry merely to make SSH connect. Investigate a mismatch first.
After strict known-host verification succeeds, run:

```sh
scripts/atlas-preflight.sh --ssh root@atlas.example-tailnet.ts.net \
  --source /mnt/user/Photos --source /mnt/user/Documents

ssh -o BatchMode=yes -o StrictHostKeyChecking=yes \
  root@atlas.example-tailnet.ts.net sh -s -- \
  --config /mnt/user/appdata/covalent/config \
  --data /mnt/user/appdata/covalent/data \
  --source /mnt/user/Photos \
  --source /mnt/user/Documents \
  --restore /mnt/user/Restore/Covalent \
  --kek /mnt/user/system/covalent-secrets/key-encryption-key \
  < scripts/validate-setup-paths.sh
```

The second command streams the repository's read-only validator to Atlas and
runs it against Atlas's filesystem. It rejects broad roots and every equal,
parent, or child overlap among writable state, sources, restore target, and KEK.
Do not run those `/mnt/user` checks on the operator computer.

It reads the immutable digest from the Unraid template and first validates the
exact intended container boundary: `/source`, `/boot-source`, and the separate
KEK file are `ro`; only config, data, and the explicit restore target are `rw`;
the container is rootless/read-only/no-capabilities; and no Docker or Tailscale
control socket is mounted. A writable source mapping fails this gate.

Over strict-host-key SSH, it verifies Docker/Tailscale availability, requires
Tailscale to be running, and checks that every IPv4 or IPv6 address returned by
MagicDNS for the requested hostname belongs to Atlas itself in `tailscale status
--json`. It requires TCP 8443 and UDP 8787 to have no current listener, and
canonicalizes every source with `readlink -e` or `realpath`. Each `--source` must be a normalized,
non-symlink path below one explicit `/mnt/user` share; `/mnt/user/system`,
appdata, dot-segment escapes, and paths enclosing the KEK fail closed. The
remote check actually opens each directory for a bounded one-entry listing
twice: once as the SSH identity, then under `setpriv --reuid=99 --regid=100
--clear-groups`, which matches the template's fixed container identity without
pulling or starting an image. It fails closed if `setpriv` is unavailable or the
SSH session is not UID 0.
It does not claim that host readability makes the host path read-only: the
validated Unraid `ro` bind is the no-write control. The preflight does not
SSH-mutate, write a source, pull an image, create a directory, or deploy
anything. Stop or move any existing listener before deployment, then rerun it.

After the container exists, inspect its actual image, user, ports, security
flags, and bind mounts. Rerun the path validator with the host `Source` values
reported by `docker inspect`; a template screenshot is not proof of the live
container.

## 6. First backup and restore check

Claim Atlas, unlock the HTTPS console, back up `/source` without a provider,
and wait for receipt confirmation. Restore to the distinct `/restore` mapping
with **Stop on conflicts** and compare the test file. Then pair a Mac or Android
device, select it explicitly for the next backup, and Verify. Use the complete
[success checklist](../getting-started.md#you-are-protected-when).

## 7. Current evidence and remaining deployment work

The historical v0.1.0 container cannot satisfy the current KEK contract.
Existing validation covers Docker on both Linux architectures and the real
Atmos-to-Waypoint music link; Waypoint supplies the Unraid runtime evidence.
Atlas remains offline. Publication and deployment of the final v0.2.1 digest
must be recorded separately from those earlier results. This runbook does not
claim a tested Atlas installation or full boot-device recovery. Preserve an
independent recovery copy before applying the published setup to that server.
