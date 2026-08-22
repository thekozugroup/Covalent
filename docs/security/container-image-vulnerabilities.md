# Container image vulnerabilities

Standing analysis of what the Grype gate (`severity-cutoff: high`, `fail-build: true`)
sees in `packaging/docker/Dockerfile`, and what is left to decide.

Measured locally with Grype against `linux/arm64` builds of this Dockerfile.
CI scans the merged multi-arch manifest, so its totals run higher than these;
the *set* of distinct findings is the same.

## Where it stands

| | high/critical |
| --- | --- |
| Caddy 2.10.2-alpine | **49** — 47 in the vendored Caddy binary, 2 in the Alpine base |
| Caddy 2.11.4-alpine | **12** — 10 in the vendored Caddy binary, 2 in the Alpine base |
| Caddy 2.11.4 built from source (current) | **2** — both in the Alpine base |

Measured with Grype 0.117.0 against locally built images. Identical counts on
`linux/arm64` and `linux/amd64`.

The bump cleared 39 findings and introduced 2 (both Go stdlib, superseded by the
same toolchain gap described below). Notably cleared: both step-ca criticals,
`GHSA-q4r8-xm5f-56gw` (unauthenticated certificate issuance via SCEP) and
`GHSA-h8cp-697h-8c8p` (ACME/SCEP authorization bypass), which mattered here
because the docs instruct operators to enrol the local CA from Caddy's PKI.
Also cleared: all 9 Caddy auth/path-bypass CVEs, all 6 `x/crypto`, both `grpc`
criticals, `quic-go`, both `go-jose`, `otel`, `nebula`, `x/net`, and 16 stdlib.

Blocker 1 below is now **cleared** — all ten of its findings are gone. Blocker 2,
the two unreachable OpenSSL findings, is the only thing between the image and a
green gate, and it is a decision rather than a fix.

## Blocker 1 — Caddy's Go toolchain (10 findings) — CLEARED

### What was wrong

`caddy:2.11.4-alpine` is built with `go1.26.3`. Eight of the ten were Go stdlib
advisories fixed in `1.26.6`; the other two were `google.golang.org/grpc v1.81.0`
(needs 1.82.1, `GHSA-hrxh-6v49-42gf`) and `golang.org/x/text v0.37.0` (needs
0.39.0, `GO-2026-5970`), both pinned low by Caddy's own `go.mod`.

No tag fixed this: 2.11.4 is the newest `x.y.z-alpine` Caddy publishes — there
is no 2.12. Waiting on upstream was the alternative, with unknown timing.

### What was done

Caddy is now compiled in its own build stage from `packaging/docker/caddy`,
against `golang:1.26.7-alpine3.23` pinned by digest. That stage replaced the
`caddy:2.11.4-alpine` stage entirely; nothing from the published Caddy image
enters the runtime image any more.

`packaging/docker/caddy` is the three-line consumer module `xcaddy` generates and
the official Caddy image builds: it imports `caddy/v2/cmd` and
`caddy/v2/modules/standard`, and nothing else. **It is not a fork and applies no
patch to Caddy.** Because it is a consumer of `caddy/v2` rather than a copy of
it, raising `grpc` and `x/text` there raises them for the whole build by ordinary
minimal version selection — no `replace` directive, no override of Caddy's
`go.mod`. Both are same-major upgrades covered by the Go compatibility promise,
which is why this did not turn out to be the "real drift" the earlier analysis
feared. Caddy itself stays at exactly v2.11.4.

Verified equivalent to the binary upstream ships:

| check | result |
| --- | --- |
| `caddy version` | `v2.11.4 h1:XKxkMTgNSizEvKG6QHue6cAsFOteU2qA61w2tKkCWi0=` — same module hash as upstream |
| `caddy list-modules` | **identical 132-module set**, diffed against `caddy:2.11.4-alpine` |
| `caddy adapt` on this repo's unchanged `Caddyfile` | **byte-identical JSON**, diffed against upstream's output |
| `caddy validate` | `Valid configuration` |

The `Caddyfile` did not need to change, and did not change.

Pinned in three independent places, so the stage cannot float:

- the toolchain image by digest;
- the Caddy source version and the entire module graph by `go.mod` + `go.sum`,
  built `-mod=readonly` — a checksum mismatch is a build failure, not a silent
  swap;
- `GOTOOLCHAIN=local`, so the `go 1.26.7` directive is enforced by the image's
  own compiler rather than satisfied by an automatic toolchain download. That
  directive is the load-bearing floor: a toolchain older than the one that fixed
  the eight stdlib advisories cannot build this module at all.

The Dockerfile additionally asserts the toolchain, the Caddy version and both
advisory bumps out of the built binary's own build info, and validates the
shipped `Caddyfile` against the shipped binary, so a regression fails the build
rather than reaching a scanner. `scripts/check-container-contract.sh` asserts the
same pins in the source tree and that the packaged binary reports v2.11.4.

### Cost

| | before | after | delta |
| --- | --- | --- | --- |
| `linux/arm64` | 66,906,624 B (63.81 MiB) | 72,346,112 B (68.99 MiB) | +5.19 MiB |
| `linux/amd64` | 72,877,056 B (69.50 MiB) | 78,636,032 B (74.99 MiB) | +5.49 MiB |

Both remain inside the 96 MiB budget with 27.0 MiB (arm64) and 21.0 MiB (amd64)
of headroom. The growth is the newer dependency graph, not the toolchain: the
Go build stage is discarded and never reaches the runtime image. The binary is
linked `-trimpath -ldflags "-s -w"`; without stripping it would be 72.3 MB rather
than 50.6 MB and the amd64 image would sit far closer to the ceiling.

## Blocker 2 — CVE-2026-14456 in libssl3/libcrypto3 (2 findings)

`libssl3` and `libcrypto3` 3.5.7-r0, from the Alpine base. Grype reports
**fix state = unknown**: no fixed version at any Alpine release.

### What the vulnerable code path actually is

Per NVD and the OpenSSL advisory of 2026-08-13, this is CWE-770,
CVSS 7.5 `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H` — availability only, no
confidentiality or integrity impact.

The bug is in **the OpenSSL QUIC server listener**. When a `Listener` SSL object
processes valid QUIC Initial packets for unknown destination connection IDs, it
allocates a channel object per packet and queues it awaiting `SSL_accept(3ossl)`.
The queue had no bound, so a peer sending Initial packets faster than the
application accepts them grows that queue without limit until the listener is
denied service. Present since OpenSSL 3.5, when the QUIC server was added. The
fix caps pending connections (default 256). The FIPS module is unaffected —
QUIC is outside its boundary.

Reaching it requires a process that (a) links OpenSSL and (b) runs OpenSSL's
QUIC *server* listener. A TLS client cannot reach it; neither can a TLS server
that is not using OpenSSL QUIC.

### Is it reachable in this image? No.

Verified by parsing the ELF headers of every binary in the built image:

| binary | interpreter | `DT_NEEDED` |
| --- | --- | --- |
| `/usr/local/bin/covalent-node` | none (static) | none |
| `/usr/local/bin/covalent` | none (static) | none |
| `/usr/local/bin/caddy` | none (static) | none |
| `/usr/bin/ssl_client` | `ld-musl` | `libssl.so.3`, `libcrypto.so.3`, `libc` |

- **Covalent's own binaries are fully static** — no interpreter, no dynamic
  section, no `DT_NEEDED`. They physically cannot load `libssl.so.3`. Their QUIC
  is `quinn 0.11.11` over `rustls 0.23.43` with `ring 0.17.14` (read out of the
  binary), and they contain zero OpenSSL symbol strings. The initial hypothesis
  — that Covalent's QUIC is Rust and does not use OpenSSL — is **correct**.
- **Caddy is a static Go binary** using Go's `crypto/tls` and `quic-go`. Zero
  OpenSSL symbol strings. Not a consumer. Re-verified after the move to a
  from-source build: still no ELF interpreter, still zero `libssl.so.3` /
  `libcrypto.so.3` references and zero OpenSSL symbols, with 701 `crypto/tls`
  references. Compiling Caddy ourselves did not change this property, so the
  reachability argument below is unaffected by that change.
- **The entrypoint** is a POSIX shell script; it invokes `covalent-node`,
  `caddy`, `mkdir` and `getent`. None link OpenSSL.
- **The only dynamic consumer in the entire image is `/usr/bin/ssl_client`**, a
  67 KB busybox helper that performs TLS *client* handshakes for
  `busybox wget https://…`. It is a client, so it never constructs a QUIC
  listener, and nothing in this image invokes it.

So `libssl.so.3` and `libcrypto.so.3` sit on disk as an unused shared library
that no process in the image loads, and the specific vulnerable path within them
is a server role that nothing here performs. The image also runs non-root,
read-only-rootfs, `--cap-drop=ALL`, `--security-opt=no-new-privileges`.

### Is a newer Alpine base fixed? No.

Queried the Alpine security database directly for every live branch:

| branch | openssl | CVE-2026-14456 fixed in |
| --- | --- | --- |
| v3.22 | 3.5.7-r0 | none |
| v3.23 (current) | 3.5.7-r0 | none |
| v3.24 | 3.5.7-r0 | none |
| edge | 3.5.7-r0 | none |

Confirmed by scanning each image: 3.23 and 3.24 both report exactly these two
findings and nothing else; `edge` reports these two **plus three unfixed busybox
highs**, so edge is strictly worse. Moving the base forward does not help.

### Can OpenSSL be removed from the image entirely?

- **`apk del` — no.** `libcrypto3` is required by `apk-tools` itself (it verifies
  package signatures) and `ssl_client` is pulled by `busybox` and
  `alpine-baselayout`. apk refuses the removal. Deleting the files by hand while
  leaving `/lib/apk/db/installed` intact would only hide the finding from the
  scanner, which is suppression wearing a disguise, not remediation.
- **`busybox:musl` — no net gain.** Genuinely carries no OpenSSL, but ships
  busybox 1.38.0 with three *unfixed* high CVEs of its own, and has no `getent`,
  which the entrypoint uses to resolve the advertised peer address. Trades two
  unreachable findings for three, and breaks a feature.
- **distroless/static or scratch — possible, but not a packaging-only change.**
  All three binaries are static, so they would run. But the image would lose
  `/bin/sh`, which the entrypoint script, the `HEALTHCHECK`, and
  `check-container-runtime.sh` all depend on. Adopting it means moving the
  entrypoint's process supervision and hostname resolution into `covalent-node`
  itself — a change in `crates/`, not `packaging/`. It is the only option that
  truly removes OpenSSL, and it is a real project, not a base-image swap.

### Recommendation — owner's decision, not taken here

No `.grype.yaml` entry has been added; that judgment is reserved.

On the evidence, CVE-2026-14456 is not exploitable in this image: an
availability-only bug in a code path that requires an OpenSSL QUIC server, in a
library that no process in the image loads, in a product whose QUIC is Rust.
There is no fix to take, and no reachable base image that carries one. The
proportionate response is a **narrowly scoped, documented ignore for
CVE-2026-14456 specifically** — not a cutoff change, not a blanket
`fail-build: false` — carrying this analysis, and a re-review trigger for when
Alpine publishes a fixed openssl or the base image changes. The alternative,
distroless, is defensible but should be chosen for its own merits rather than to
clear a finding that is already unreachable.

**This decision now does unblock the gate.** When this analysis was first
written, Blocker 1 stood behind it — ten findings no decision here could waive.
Those are gone. These two are all that is left between the image and a green
`severity-cutoff: high` gate, so the scoped-ignore judgment is no longer
academic: taking it turns the lane green, and declining it keeps the lane red on
two findings that have no fix to take and no reachable base image that carries
one.

That is precisely why it is still **not taken here**. Being the last thing in the
way is not an argument for waiving it, and a gate should never be quietly
loosened by whoever happens to be standing next to it. No `.grype.yaml` entry has
been added, `severity-cutoff` is unchanged at `high`, and `fail-build` is
unchanged at `true`.

## Process gaps

**Grype ran only in the release lane.** `ci.yml`'s `container-foundation` never
scanned, which is why 60 findings were invisible for this project's entire life
and surfaced for the first time at tag time. The step below is written and ready
but **still not landed**: Blocker 1 has cleared, so it would now red every merge
to `main` on exactly the two OpenSSL findings and nothing else. It should go in
as soon as Blocker 2 is decided — appended to `container-foundation`, after the
existing `docker-compose-e2e.sh` step:

```yaml
      - uses: anchore/scan-action@1638637db639e0ade3258b51db49a9a137574c3e # v6
        with:
          image: covalent:ci
          fail-build: true
          severity-cutoff: high
```

Same action, pin, cutoff and failure mode as the release lane, so CI and the
release gate cannot disagree about what is acceptable.

**The SBOM outran its own gate.** `anchore/sbom-action` defaults
`upload-release-assets` to `true`, and it ran *before* `anchore/scan-action`.
On the v0.1.0 tag that attached an SBOM to the GitHub Release describing an
image that then failed the scan and was never signed, attested, or pushed. Fixed
in `container-supply-chain.yml`: the scan now runs first, and both of the SBOM
action's upload paths are disabled so the only route to the release page is the
explicit publish step that runs after the gate. The stray asset was deleted from
the v0.1.0 draft.
