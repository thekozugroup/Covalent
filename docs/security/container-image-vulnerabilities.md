# Container image vulnerabilities

Standing analysis of what the Grype gate (`severity-cutoff: high`, `fail-build: true`)
sees in `packaging/docker/Dockerfile`, and what is left to decide.

Measured locally with Grype against `linux/arm64` builds of this Dockerfile.
CI scans the merged multi-arch manifest, so its totals run higher than these;
the *set* of distinct findings is the same.

## Where it stands

| | high/critical |
| --- | --- |
| Before (Caddy 2.10.2-alpine) | **49** — 47 in the vendored Caddy binary, 2 in the Alpine base |
| After (Caddy 2.11.4-alpine) | **12** — 10 in the vendored Caddy binary, 2 in the Alpine base |

The bump cleared 39 findings and introduced 2 (both Go stdlib, superseded by the
same toolchain gap described below). Notably cleared: both step-ca criticals,
`GHSA-q4r8-xm5f-56gw` (unauthenticated certificate issuance via SCEP) and
`GHSA-h8cp-697h-8c8p` (ACME/SCEP authorization bypass), which mattered here
because the docs instruct operators to enrol the local CA from Caddy's PKI.
Also cleared: all 9 Caddy auth/path-bypass CVEs, all 6 `x/crypto`, both `grpc`
criticals, `quic-go`, both `go-jose`, `otel`, `nebula`, `x/net`, and 16 stdlib.

**The gate does not pass at 12.** Two separate blockers remain, and the second
is the smaller one.

## Blocker 1 — Caddy's Go toolchain (10 findings)

`caddy:2.11.4-alpine` is built with `go1.26.3`. Eight of the ten are Go stdlib
advisories fixed in `1.26.6`; the other two are `google.golang.org/grpc v1.81.0`
(needs 1.82.1) and `golang.org/x/text v0.37.0` (needs 0.39.0), both pinned by
Caddy's own `go.mod`.

Nothing in this repository can fix these by choosing a different tag: 2.11.4 is
the newest `x.y.z-alpine` Caddy publishes — there is no 2.12 — and it is the
newest available at time of writing. The options are:

1. **Wait for the next upstream Caddy release** rebuilt on a current Go
   toolchain. Lowest effort, no drift, unknown timing.
2. **Build Caddy from source** in the builder stage against a pinned current Go
   toolchain. Clears the eight stdlib findings. Does *not* clear grpc or x/text
   without additionally overriding Caddy's `go.mod`, which is real drift from a
   binary upstream tests and signs. Adds Go to the build and lengthens it.

This is a genuine dependency on upstream, not a judgment call, and it is the
reason the gate stays red regardless of what is decided about the base image.

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
  OpenSSL symbol strings. Not a consumer.
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

**Whichever way that goes, it does not unblock the gate on its own.** Blocker 1
is ten findings that no decision here can waive.

## Process gaps

**Grype ran only in the release lane.** `ci.yml`'s `container-foundation` never
scanned, which is why 60 findings were invisible for this project's entire life
and surfaced for the first time at tag time. The step below is written and ready
but **deliberately not landed**, because it would red every merge to `main` on
the twelve findings above. It should go in immediately after Blocker 1 clears
and Blocker 2 is decided — appended to `container-foundation`, after the
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
