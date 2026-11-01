# Container image vulnerabilities

Standing analysis of what the Grype gate (`severity-cutoff: high`, `fail-build: true`)
sees in `packaging/docker/Dockerfile`, and what is left to decide.

Current CI scans the immutable local image ID separately on native amd64 and
arm64 runners. Historical measurements below retain their original scope.

## 2026-11-01 JOSE and HTML remediation

The Caddy consumer now pins JOSE v3 to the official upstream security backport
`v3.0.6-0.20261030183946-0e502b87c7aa`, and v4 to `v4.1.7`. The internal CA
imports v3 through Smallstep, so updating only v4 or Caddy is insufficient.
The [upstream v3 change](https://github.com/go-jose/go-jose/commit/0e502b87c7aa651155b11ae8e195ec33ce10230a)
bounds RSA parameters and rejects private keys embedded in untrusted headers.
It is an exact official commit, not a released v3 tag or local fork.
The [v4.1.7 release](https://github.com/go-jose/go-jose/releases/tag/v4.1.7)
includes the later v4 fixes. Docker runs eight rejection cases across both major
versions and independently checks both versions in the actual binary metadata.

The current database also exposed [GO-2026-6860](https://pkg.go.dev/vuln/GO-2026-6860)
and [GO-2026-6866](https://pkg.go.dev/vuln/GO-2026-6866), both fixed in
`golang.org/x/net v0.62.0`. Their affected HTML package is actually compiled in,
so these were fixed despite having Unknown scanner severity. The required
transitive versions move to x/crypto0.58.0, x/text0.43.0, x/sys0.49.0,
x/sync0.24.0, x/term0.47.0 and x/tools0.51.0. The Caddy compatibility snapshot,
Go1.26.10, CEL, gRPC, compression and OpenTelemetry selections remain pinned.

Exact stripped Linux amd64 and arm64 binaries were built locally with CGO off,
trimpath and no VCS stamping. Syft1.51.1 identifies149 packages per ELF, including
the exact two JOSE versions. Grype0.117.0 with schema6.1.10, database built
2026-11-01T06:20:12Z, reports zero High/Critical findings on each binary.
One Unknown module-level match remains: GO-2026-5932 in x/crypto. The advisory
names OpenPGP packages absent from both actual compiled package graphs. The match
is retained in the raw reports, with no exclusion, VEX, only-fixed filter or
severity-gate change. This is not a claim of zero scanner findings.

The normal eight JOSE cases, upstream HTML tests, eighteen evidence tests and
source container contract pass. Eight host runtime checks cover generated-CA
HTTPS, security headers, static assets, event-stream response, owner-only Unix
ingress and unchanged CA key/certificate bytes after restarting Caddy. Host
runtime testing relocates only paths/ports and disables trust-store installation;
it does not establish native Linux/container execution. Source/license reference
pins were regenerated from both real target graphs and ELF files, with unchanged
reviewed CA metadata modeled separately. No synthetic installed APK receipt is
presented as runtime proof. Native image builds must regenerate against their
actual installed package databases and pass the unchanged complete-image scan.

Bounded raw scan/SBOM, input hashes and scope are retained in
`artifacts/evidence/caddy-jose-20261101/`. No Pi/phone update, service restart,
account change, import consent or sync-journal reset is part of this change.

## 2026-10-20 remediation in progress

The October database refresh rejects the previous Go 1.26.7 workers and
`golang.org/x/net` v0.58.0 in Caddy. The maintained source now selects exact
Go 1.26.10, x/net v0.60.0, x/crypto v0.57.0 and OpenTelemetry v1.45.0, with
the log SDK and all log exporters aligned at v0.21.0. The trace exporters
also select v1.45.0. These address the current standard-library/HTTP2 findings,
[log SDK advisory](https://github.com/open-telemetry/opentelemetry-go/security/advisories/GHSA-hjf4-fphr-2h65)
and [exporter logging advisory](https://github.com/open-telemetry/opentelemetry-go/security/advisories/GHSA-8wmf-6v46-5gfg).
[Go's release history](https://go.dev/doc/devel/release#go1.26.10) records the
October 19 certificate-verification fix included in 1.26.10.

The Go builder is pinned to the verified official multi-platform index
`sha256:a07f231c19b03e6106cf442f02a5bcf262147e744d15b7b5e4b6d81d420c70a0`.
The build now explicitly installs the same official signed, noarch
`ca-certificates-bundle=20261020-r0` APK on both targets. Its 107,116 bytes are
pinned to SHA-256 `a4c5de1bf37652cd98cbe71aec209512ae0508a964961957a6c9650f795e46c4`.
The resulting 178,791-byte bundle hashes to
`5961c0a3c65459931c023e0075c09733f09aea4ca439cb85bc4a08bb4f3f568b`.
The architecture-independent package reached the x86 mirror while ARM's index
still offered the previous version. The checksum-pinned build-only input
avoids that mirror race. Signature checking uses `/usr/share/apk/keys` per
command, including the official x86 signer; the global trust store is unchanged.
No untrusted-package bypass is used.

The Go builder's old full `ca-certificates` generator is removed first because
its old source certificates could overwrite the new bundle on a later apk
trigger. The signed bundle alone supplies Caddy's required trust file. Real
upgrades on both digest-verified Go base layers pass; exact bundle bytes and
installed records were measured. Apk 3.0.8 records the signed noarch input as
`x86_64` on amd64 and `noarch` on arm64; target-specific checks bind those
observed values instead of accepting either. Complete recipe tree
`ac857c298f6fd212498ce76bb87836d940cdba2d` and the exact GitLab source archive
were freshly downloaded and verified against Git blobs and APKBUILD SHA-512.
All 155 source inputs validate. Notice/source review pins were regenerated
without creating a synthetic installed-runtime receipt. Full native CI image
checks and vulnerability scans remain required.

The final locally rebuilt Caddy ELF pair passes its strict distribution
verifier. Explicit file SBOM scans with Grype 0.117.0, database v6.1.10 built
2026-10-20T06:29:49Z, inventory 149 packages per ELF and each report one
unknown-severity `GO-2026-5932` module finding, with no critical/high findings.
These scans supersede the earlier directory-based zero result; a scan must
demonstrate that it inventoried the intended binaries before being accepted.
The [Go advisory](https://vuln.go.dev/ID/GO-2026-5932.json) affects the deprecated
`golang.org/x/crypto/openpgp` packages. Neither exact Caddy target graph imports
those packages (949/951 packages inspected). No finding is filtered or ignored.
Arm64 was compiled natively in Linux; amd64 was cross-compiled. This source-graph
assessment excludes Alpine and other runtime executables and is not a complete
image clearance. Explicit Mac/Android rclone scans retain the same unknown row;
their target graphs also contain no OpenPGP imports.

Hosted run 39485175979 exposed the old CA pin failure. The source now contains
the coordinated builder/runtime/recipe/source/notice fix above. Strict byte,
package and vulnerability gates remain intact; a pushed fix is not image
acceptance until the new native jobs pass.

Hosted run 39490316340 confirms the signed CA upgrade, exact trust bytes and
Caddy collection on both native runners, then stops in source retrieval with a
generic diagnostic. A clean local Linux container fetches/verifies all 155
inputs. The collector now reports sanitized per-input HTTP/network failures,
without exception URLs or bodies. The exact, unchanged 230,531-byte CA source
archive is retained in the repository and seeds the build-only cache; size,
SHA256 and original APKBUILD SHA512 checks remain mandatory. This removes the
GitLab download dependency, without assuming an unrecorded HTTP cause or
claiming final image acceptance.

The same refresh also exposed RUSTSEC-2026-0382 and RUSTSEC-2026-0383 in the
shared TLS verifier/root bundle. The lockfile now selects rustls-webpki
0.103.16 and webpki-root-certs 1.0.10; current cargo-deny advisory, ban, license
and source checks pass without exceptions.

CI run 39481535802 separately exposed an amd64 timeout in optional source
deletion propagation after its first eight folder checks passed. Its arm64
folder journey passed. Keep that failure open for investigation; deletion
remains disabled for appliance links. The historical BusyBox scanner rows
below remain visible. No scan ignore or release approval is added here.

## Where it stands

| | high/critical |
| --- | --- |
| Caddy 2.10.2-alpine | **49** — 47 in the vendored Caddy binary, 2 in the Alpine base |
| Caddy 2.11.4-alpine | **12** — 10 in the vendored Caddy binary, 2 in the Alpine base |
| Caddy upstream snapshot `v2.11.5-0.20260711231708-b2693fb63a30` built from source, with exact Alpine security revisions (checkpoint 38) | **1** — `GHSA-vp52-pcj8-j9qc` in gRPC v1.82.1 |
| Same source and toolchain with gRPC v1.83.1 (checkpoint 39) | **0** in the image scan; dependency review separately found `GHSA-2v4p-qf9q-27wj` |
| Same source and toolchain with gRPC v1.83.2 and the local BusyBox backport (checkpoint 44) | **0** on arm64; current amd64 scan skipped after the image-size failure |

The checkpoint 38 row is from Grype 0.117.0 database v6.1.9, built
2026-09-08T06:30:10Z, against the exact CI images for both architectures. The
candidate has passed source, module, binary-build-info, and Caddy configuration
validation. It is not recorded as clear until the release workflow repeats the
strict scan against both rebuilt exact images.

The bump cleared 39 findings and introduced 2 (both Go stdlib, superseded by the
same toolchain gap described below). Notably cleared: both step-ca criticals,
`GHSA-q4r8-xm5f-56gw` (unauthenticated certificate issuance via SCEP) and
`GHSA-h8cp-697h-8c8p` (ACME/SCEP authorization bypass), which mattered here
because the docs instruct operators to enrol the local CA from Caddy's PKI.
Also cleared: all 9 Caddy auth/path-bypass CVEs, all 6 `x/crypto`, both `grpc`
criticals, `quic-go`, both `go-jose`, `otel`, `nebula`, `x/net`, and 16 stdlib.

The two original blockers below remain **cleared**. Later database refreshes also exposed
GO-2026-5158 in OpenTelemetry v1.43.0 and GO-2026-6094 in cel-go v0.29.2;
the consumer module pins patched v1.44.0 and v0.30.0 respectively, and the
Dockerfile verifies both selections from the built Caddy binary.

## Current BusyBox backport and remaining review

Checkpoint 44 arm64 image
`sha256:1167d99930768d026f3db208cfa449b71852b34a99aa5063fb4f3c48d10ee67f`
passes its source/notice verifiers and all nine packaged folder-sync checks.
Grype 0.117.0 database v6.1.9, built 2026-09-12T06:27:25Z, reports zero High
and three Medium CVE-2025-60876 findings for `busybox`, `busybox-binsh` and
`ssl_client`, each at the local version `1.37.0-r1000`.

The final build includes the reviewed request-target patch, verifies all
45 recipe patches, passes all 13 wire checks and binds installed payloads to
the tested binaries. Its complete original/modified recipe and corresponding
source pass the packaged verifier; see [Alpine source evidence](alpine-runtime-source.md).
The scanner report lists no fixed Alpine version for this advisory. Independent
review of this immutable arm64 image concludes that the applicable request-target
behavior is remediated by the local backport. `busybox-binsh` is a symlink package
depending on the same patched executable. `ssl_client` forwards TLS bytes and
does not construct HTTP request targets; its finding comes from the shared
BusyBox package origin. This is an image-specific technical assessment, not a
claim of an Alpine or upstream fixed release. All three scanner rows remain
visible and no ignore rule or VEX filter is introduced.

The review binds the frozen upstream source and original/modified Alpine recipe
to all 45 applied patches, the 13 direct/proxy wire checks, and the final runtime's
successful comparison with the tested payload digests. The final image inherits
that runtime without replacing its BusyBox files. Ordinary job console evidence
is retained at `artifacts/validation-2026-09-12/checkpoint44-hosted/103575517205.log`;
the independent review is `checkpoint44-busybox-applicability-review/result.json`.
Its SHA-256 is `2f60d884cfb1fcf4eaca0237fa6aff1025844d59a2dcf4f0f752828e315969f1`.

The earlier console reports successful digest comparisons without printing their
values. The build now prints the verified digest list, allowing a subsequent
clean builder's unsigned payloads to be compared numerically. That reproducibility
comparison and the amd64 applicability review remain open. No new runtime files,
package versions, image-size limits or scanner policy are introduced by logging
the existing checked digest list.

The current amd64 image passes its source and basic runtime checks but exceeds
the unchanged 128 MiB size cap, so its later scan does not run. The next CI
checkpoint places the size gate after functional and security checks. All
size failures still fail the job; the reorder permits separate evidence for
functionality, security and image size.

## gRPC advisories — v1.83.2 exact-image coverage

The checkpoint 38 scans found
[`GHSA-vp52-pcj8-j9qc`](https://github.com/advisories/GHSA-vp52-pcj8-j9qc) /
CVE-2026-84304 in `google.golang.org/grpc` v1.82.1. The advisory affects
versions through v1.83.0 and identifies
[v1.83.1](https://github.com/grpc/grpc-go/releases/tag/v1.83.1) as the patched
release. Checkpoint 39 selected it and both exact-image Grype scans reported no
High finding. The dependency-review database then identified
[`GHSA-2v4p-qf9q-27wj`](https://github.com/advisories/GHSA-2v4p-qf9q-27wj), a
separate xDS server panic affecting v1.83.1. The exact compiled Caddy graph does
not include `grpc/xds` or `internal/xds/server`, but no reachability waiver is
used: the consumer selects the signed patched
[v1.83.2](https://github.com/grpc/grpc-go/releases/tag/v1.83.2), and the Docker
build rejects every other gRPC version in the built binary metadata. This is a
dependency update, not a scanner suppression or severity-policy change.

Ordinary Go minimal-version selection also advances the exact transitive graph
required through v1.83.2. Relative to v1.83.1, it advances `x/crypto` to v0.55,
`x/mod` to v0.38, `x/net` to v0.58, `x/sync` to v0.22, `x/sys` to v0.47,
`x/term` to v0.45, `x/text` to v0.41 and `x/tools` to v0.48. The checked-in
`go.sum` authenticates the complete selected graph. The exact module diff and
local binary measurements are retained in the checkpoint validation artifacts.
With identical Go 1.26.7 Linux build flags, v1.83.2 adds 8,192 bytes to the
v1.83.1 stripped amd64 binary and no bytes to arm64. Checkpoint 39 had only
72,192 bytes of amd64 image headroom. The measured binary-only increase leaves
an estimated 64,000 bytes before image-layer accounting; the actual rebuilt
image remains authoritative. The 128 MiB budget is unchanged. Both release architectures still need an exact
rebuilt-image Grype result before this finding can be marked cleared.

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

`packaging/docker/caddy` imports `caddy/v2/cmd` and the upstream modules required
by the shipped Caddyfile and runtime checks. It omits unrelated modules in the
standard distribution. No official Caddy patch release
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
dependency findings after the initial CEL compatibility work. The configured
distribution no longer compiles `github.com/go-chi/chi/v5`, removing its three
findings with the unused standard modules that selected it. The consumer raises
the two remaining compiled dependencies through ordinary minimal version
selection, without a `replace`, fork, or source patch:

| module | prior | selected | finding |
| --- | --- | --- | --- |
| `github.com/google/cel-go` | `v0.29.2` | `v0.30.0` | GO-2026-6094 |
| `github.com/klauspost/compress` | `v1.18.6` | `v1.18.7` | GO-2026-5841 |

Pinned Go 1.26.7 `go mod verify`, `go test ./...`, `go vet ./...`, the custom
build, and `caddy validate` all pass with that graph. Official
`golang.org/x/crypto` is pinned to `v0.56.0`, fixing
[GO-2026-6354](https://pkg.go.dev/vuln/GO-2026-6354) and
[GO-2026-6355](https://pkg.go.dev/vuln/GO-2026-6355) in the imported SSH package.
`govulncheck v1.7.0` source analysis on 2026-09-12 reports zero called or
imported-package vulnerabilities. It retains GO-2026-5932 at module level for
`golang.org/x/crypto/openpgp`: that unmaintained package is neither imported nor
called, and the report supplies no fixed version. No finding is suppressed. A stripped binary scan cannot
recover call information and may conservatively report unreachable code; the
Go tool's documented limitation says binary mode can produce that false
positive. Release review therefore retains the exact source call graph plus an
unstripped verification build.

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
| `caddy list-modules` | **112 modules** in the configured distribution |
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

**Container scans now run before release.** Both ordinary CI architecture
jobs scan their exact locally built image ID after runtime acceptance, retaining
JSON evidence even when the scan fails. High and critical findings fail the job;
findings without an available fix remain included. A missing report also fails.
No image is published by these jobs. The evidence-copy step streams Docker's
archive through GNU tar so read-only image directories regain their final modes
only after their files are extracted. Distribution files have their own `engine/`
subdirectory, leaving the outer evidence directory writable for image metadata.
An ordinary-user isolated Atmos fixture
verifies exact bytes and restored 0555/0444 modes; its temporary root was removed.
This fixes checkpoint 35's copy failure without changing image permissions.

CI and release scans use `scripts/run-pinned-grype-scan.sh`, which downloads the
official Grype v0.117.0 release archive for the runner architecture and checks an
exact hard-coded SHA-256 before extracting only its bounded regular-file contents.
The pinned hashes are `38525dab...9c26` for Linux amd64 and
`935f628b...91b` for Linux arm64, matching the release's
[official checksum asset](https://github.com/anchore/grype/releases/download/v0.117.0/grype_0.117.0_checksums.txt).
The workflow does not execute an installer or a GitHub action's mutable bundled
download code. The scanner verifies the database hash and age, requires a live
update check, includes findings without fixes, and uses no ignore rules or VEX.
The private scanner directory has a 4 GiB disk allowance and a 12-minute scan
deadline. On 2026-09-30, the database, scanner, and update files occupied
3,259,088,896 bytes on amd64 and 3,249,950,720 bytes on arm64, exceeding the
previous 3 GiB allowance despite complete scans with no high or critical
findings. The larger storage allowance accommodates database growth; excess
still fails, and vulnerability, freshness, and integrity checks are unchanged.
Grype's documented `docker:` source selects the local Docker daemon, and a
bounded validator rejects a JSON report unless both its requested input and
resolved image ID equal the immutable ID recorded before the scan. The release
lane attempts both architectures and uploads each JSON report before a combined
gate can permit SBOM generation or publishing. These changes require a fresh
hosted run before claiming the current complete images pass.

**The SBOM outran its own gate.** `anchore/sbom-action` defaults
`upload-release-assets` to `true`, and it ran *before* `anchore/scan-action`.
On the v0.1.0 tag that attached an SBOM to the GitHub Release describing an
image that then failed the scan and was never signed, attested, or pushed. Fixed
in `container-supply-chain.yml`: the scan now runs first, and both of the SBOM
action's upload paths are disabled so the only route to the release page is the
explicit publish step that runs after the gate. The stray asset was deleted from
the v0.1.0 draft.


## 2026-10-31: bind the musl security revision to its source

CI41220252198 reached the Alpine evidence gate after the Python pin refresh and
BusyBox tests. Installing the pinned OpenSSL libraries selected musl1.2.5-r24,
which the older runtime lock correctly rejected. Pin musl and musl-utils together
to1.2.5-r24 and retain the installed-version assertions and exact package/commit
comparison. The source lock now binds the full [Alpine security recipe](https://gitlab.alpinelinux.org/alpine/aports/-/commit/fd05fc094264bce4fb76efc6c7f61ba4abfcb609),
including its APKBUILD checksums, Git blobs/tree and newly referenced patches.
Unrelated source origins, notice bounds and runtime-removal policy are unchanged.

Actual runtime-base APK databases match the reviewed non-BusyBox runtime records
on asserted aarch64 and x86_64 containers, excluding the documented build-only
apk-tools/libapk/zlib removal. Source/notice/bundle generation also passes against
real checksum-locked sources using explicitly synthetic installed/builder rows;
that separate check is not a final-image build or runtime acceptance. Rust/Cargo
inputs are unchanged from3ebbecf. Sanitized bindings and limits are recorded in
`artifacts/evidence/security-refresh/runtime-provenance.json`. Full exact-head CI
and immutable image scanning remain release gates.
