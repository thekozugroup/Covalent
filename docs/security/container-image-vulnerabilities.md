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
| Caddy upstream snapshot `v2.11.5-0.20260711231708-b2693fb63a30` built from source, with exact Alpine security revisions (current) | **0** |

Measured with Grype 0.117.0 against locally built images. The final current-row
measurement is the local `linux/arm64` candidate; the release workflow repeats
the same strict scan independently on both architectures before promotion.

The bump cleared 39 findings and introduced 2 (both Go stdlib, superseded by the
same toolchain gap described below). Notably cleared: both step-ca criticals,
`GHSA-q4r8-xm5f-56gw` (unauthenticated certificate issuance via SCEP) and
`GHSA-h8cp-697h-8c8p` (ACME/SCEP authorization bypass), which mattered here
because the docs instruct operators to enrol the local CA from Caddy's PKI.
Also cleared: all 9 Caddy auth/path-bypass CVEs, all 6 `x/crypto`, both `grpc`
criticals, `quic-go`, both `go-jose`, `otel`, `nebula`, `x/net`, and 16 stdlib.

Both blockers below are now **cleared**. Later database refreshes also exposed
GO-2026-5158 in OpenTelemetry v1.43.0 and GO-2026-6094 in cel-go v0.29.2;
the consumer module pins patched v1.44.0 and v0.30.0 respectively, and the
Dockerfile verifies both selections from the built Caddy binary.

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
`caddy/v2/modules/standard`, and nothing else. No official Caddy patch release
newer than v2.11.4 is published yet. To compile against patched cel-go, the
module pins upstream snapshot `v2.11.5-0.20260711231708-b2693fb63a30`, commit
`b2693fb63a30e6d7be0972c3645e9a2c0a500e93`, whose direct purpose is the cel-go
`v0.29.2` upgrade and two `InterpretableV2` argument updates. The consumer
module now selects compatible cel-go `v0.30.0` because GO-2026-6094 affects
v0.22.0 through v0.29.x. This is an upstream snapshot, not a fork, but it is 33
commits after v2.11.4 and therefore is not claimed to be v2.11.4. The complete
intervening commit range is listed below so the runtime delta is explicit.

### Upstream snapshot delta reviewed

The pinned commit is the 33rd commit after the v2.11.4 tag. The 32 predecessor
commits are listed below, followed by the pinned `b2693fb6` CEL compatibility
commit itself. Their upstream subjects are retained here for release review:

```text
fcc7860d reverseproxy: replace placeholders specified for sni while using http3
915793f6 caddyhttp: add {http.request.proto_name} placeholder for spec-compliant protocol names
3b7bde8f httpcaddyfile: error on duplicate named_routes
d730df2a cmd: colored error message in WrapCommandFuncForCobra
d3986f82 Add missing "is"
55b3397a reverseproxy: validate on weighted_round_robin loadbalancing policy
4fd8c87f caddyhttp: Default max_header_bytes to 16 KiB
0f7f8e9c forwardauth: error on duplicate uri subdirective
997d3f6b encode: add standard benchmark and conformance harness
fcba554d caddyhttp: New expected_underscore_headers server option
25b3eab6 Merge commit from fork
52dc6709 rewrite: fix wrong index check in trimPathPrefix
16235cce intercept: fix replace_status being silently dropped
39c9a85f fileserver: append repeated hide subdirectives instead of overwriting
ae9bc028 rewrite: scope keyed query replace to its named key
4dbe0a93 readme: Update logo
ab56721a Remove -v from tests
d2e0ad1e reverseproxy: log status 499 instead of 0 when client disconnects
69d6ace3 tracing: fix BatchSpanProcessor goroutine leak on config reload
6ab855d3 browse: Update Caddy logo
30f0ddd9 caddyhttp: Document dropping underscore headers
57603822 caddyhttp: Clean up variable scope in vars matcher
51a4bde1 Update human and agent contributing guidelines
f4500684 http: normalize method names to uppercase in MatchMethod.Provision
13a4c3f4 caddyhttp: add URL pattern request matcher
08ad0641 caddyhttp: fix escaped path matcher over-matching longer paths
4e620952 core: preserve metrics registry in Context.WithValue
75c988d1 reverseproxy: compare sticky-session cookie hash in constant time
945d1997 reverseproxy: fix misleading handle_response error for extra matcher args
1830809a reverseproxy: save dial info in a context key instead of a variable key
c6180a08 caddyhttp: fix path_regexp (MatchPathRE) Windows backslash bypass
c1907df2 intercept: fix misleading handle_response error for extra matcher args
b2693fb6 build(deps): bump cel-go from v0.28.1 to v0.29.2
```

This tradeoff is deliberate and bounded: the source is fetched by its canonical
Go pseudo-version, verified by `go.sum`, and the build rejects any other Caddy
snapshot. The local gates run `go test ./...`, `go vet ./...`, custom binary
build metadata checks, `caddy validate` against the shipped Caddyfile, and the
container contract check. A future official patch release should replace this
snapshot after repeating the same gates and delta review.

### Post-snapshot dependency refresh

A refreshed Go vulnerability database on 2026-08-28 added five fixable
dependency findings after the initial CEL compatibility work. The consumer
module now raises them through ordinary minimal version selection, without a
`replace`, fork, or source patch:

| module | prior | selected | finding |
| --- | --- | --- | --- |
| `github.com/google/cel-go` | `v0.29.2` | `v0.30.0` | GO-2026-6094 |
| `github.com/go-chi/chi/v5` | `v5.2.5` | `v5.3.0` | GO-2026-5774, GO-2026-5775, GO-2026-5777 |
| `github.com/klauspost/compress` | `v1.18.6` | `v1.18.7` | GO-2026-5841 |

Pinned Go 1.26.7 `go mod verify`, `go test ./...`, `go vet ./...`, the custom
build, and `caddy validate` all pass with that graph. Official
`govulncheck v1.7.0` source analysis reports zero called vulnerable symbols.
It reports GO-2026-5932 only at module level because `golang.org/x/crypto` is in
the graph: the affected `openpgp` packages are not imported or called, and the
Go report has no module version that can fix an unused package. A stripped
binary scan cannot recover call information and may conservatively report
unreachable code; the Go tool's documented limitation says binary mode can
produce that false positive. The release decision therefore uses the exact
source call graph plus an unstripped verification build, not a suppression or
an ignored advisory.

The full upstream integration package was compared under the same local
environment. `TestH2ToH1ChunkedResponse` fails against both the exact v2.11.4
module and this pinned snapshot with the same fixture-side 404, so it is a
pre-existing test-environment/fixture failure rather than a snapshot regression.
The CEL, HTTP Caddyfile, fileserver, reverse-proxy, encoding, tracing, PKI, TLS,
and related targeted packages pass for the pinned snapshot.

Verified against the binary upstream ships, with the one intentional upstream
snapshot delta called out explicitly:

| check | result |
| --- | --- |
| `caddy version` | `v2.11.5-0.20260711231708-b2693fb63a30 h1:GLKxfFw6+vJgw57aRSkZwXiogAFn4JMb6wqIop4KJtY=` — exact pinned upstream snapshot |
| `caddy list-modules` | **133 standard modules**; the snapshot adds the upstream `http.matchers.url_pattern` module compared with the 132-module v2.11.4 image |
| `caddy adapt` on this repo's unchanged `Caddyfile` | **valid configuration**, checked against the pinned snapshot |
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

The Dockerfile additionally asserts the toolchain, the Caddy version and all
dependency advisory bumps out of the built binary's own build info, and validates the
shipped `Caddyfile` against the shipped binary, so a regression fails the build
rather than reaching a scanner. `scripts/check-container-contract.sh` asserts the
same pins in the source tree and that the packaged binary reports the exact
upstream snapshot pseudo-version.

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

## Blocker 2 — resolved: Alpine OpenSSL security revision

The pinned Alpine 3.23.5 base contains `libssl3` and `libcrypto3` `3.5.7-r0`.
On 2026-08-25 Alpine published `3.5.8-r0` to the stable v3.23 repository, after
that base image was assembled. A fresh Grype 0.117.0 scan on 2026-08-28 found
six fixed high-severity advisories in each of those two installed packages:
CVE-2026-14457, CVE-2026-18798, CVE-2026-54874, CVE-2026-63072,
CVE-2026-63075, and CVE-2026-63076.

The runtime stage now retains the reviewed base digest and upgrades only those
two libraries to the exact signed Alpine version `3.5.8-r0`. The package
version is also an OCI label and a static/runtime contract assertion. This is
fail closed: if Alpine's signed repository cannot supply that exact revision,
the build stops rather than floating to another package. No scanner exception
or severity change is used.

### Historical reachability evidence for CVE-2026-14456

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

### Was that historical OpenSSL QUIC path reachable in this image? No.

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

### Is the pinned base itself fixed? Not yet; its stable repository is.

The official `alpine:3.23` tag still resolves to 3.23.5 and the reviewed digest
`sha256:fd791d74...daf40`, assembled with OpenSSL `3.5.7-r0`. Alpine's v3.23
package repository now supplies signed `3.5.8-r0` packages for every supported
image architecture. Pinning those two security revisions preserves the known
base filesystem while taking the available fixes immediately; the next Alpine
point-release digest can replace this narrow package layer after independent
multi-architecture scanning.

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

### Resolution

Take the signed fix, retain the prior reachability analysis only as historical
evidence, and keep the high-severity gate strict. No `.grype.yaml` exists,
`severity-cutoff: high` and `fail-build: true` remain unchanged, and the exact
package contract prevents the remediation from silently regressing.

## Process gaps

**Grype ran only in the release lane.** `ci.yml`'s `container-foundation` did not
scan, which let dependency findings wait until tag time. With both image
blockers resolved, the same pinned high-severity scan belongs in ordinary CI
after `docker-compose-e2e.sh`:

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
