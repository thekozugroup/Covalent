# Caddy compiled-target license and source evidence

This is an engineering inventory, not legal advice or release approval. It
records the exact texts observed in the pinned Caddy consumer's compiled Linux
graphs and identifies separately retained source material.

## November 1 source reference

Go1.26.10 now selects the official JOSE v3 security backport0e502b87c7aa,
JOSEv4.1.7 and x/netv0.62.0 with its required transitive updates. Both actual
Linux target graphs still contain148 modules and zero unclassified license
texts:951 packages on amd64 and949 on arm64. License-family counts and the MPL
source member manifest remain unchanged. The new readable notice SHA-256 is
`e0d4c4038811b7a94a803cb8d527f0ebf5b4477de8f36395eabffdc6d509a960`.
The strict collector/verifier bind the new consumer checksums, inventories and
actual ELF hashes. The unchanged main.go hash remains independently enforced.

This local collection is source/reference qualification. CA provenance uses the
unchanged reviewed source record, not a newly installed APK database. The exact
checksum-pinned official CA package and bundle bytes were downloaded separately;
no fresh package-signature installation is claimed. Native CI must collect and
verify actual installed metadata on each target before release. Full image
scans remain required. See the [security record](container-image-vulnerabilities.md)
and `artifacts/evidence/caddy-jose-20261101/` for input/scan hashes and limits.

## October 20 collection (historical)

The security refresh uses exact Go 1.26.10 and the same Caddy source snapshot,
with x/net v0.60.0 and OpenTelemetry v1.45.0/log modules v0.21.0. An isolated
Linux arm64 container built the arm64 binary natively and cross-compiled amd64.
Both target inventories contain 148 modules and 203 candidate license/notice
files, with zero unclassified files. Package counts are 949 for arm64 and 951
for amd64. The MPL corresponding-source member manifest is unchanged.

The new readable notice SHA-256 is
`300d0eb6042939a5d8f82f0df3c38f49abab8cfc5fe73b0e982500ac107a01fa`.
The strict verifier retains independent target counts and inventory/binary
digests in `scripts/verify-caddy-distribution-evidence.py`. The CA source is
the signed 20261020-r0 noarch package explicitly installed on the digest-pinned
Go builder after removing its old certificate generator. The Alpine source lock
retains the full matching recipe and corresponding source. Actual installed
metadata is bound separately for each target, including apk's x86 normalization.

The family counts are Apache-2.0 61, BSD-2-Clause 4, BSD-3-Clause 56, CC0-1.0
1, MIT 56 and MPL-2.0 1. Counts overlap. Final native-runner image validation
and scans remain required; local cross-compilation is not that acceptance.
The earlier collection below is retained as historical evidence.

## Exact build binding

The shipped Caddy binary is built from the consumer in
`packaging/docker/caddy`. It imports the upstream modules required by the
shipped Caddyfile and runtime checks: the Caddyfile adapter, events, HTTP,
gzip, zstd, headers, reverse proxy, internal PKI, TLS, standard session-ticket
keys, file storage and logging. It uses upstream snapshot
`v2.11.5-0.20260711231708-b2693fb63a30` and Go 1.26.7, sets `CGO_ENABLED=0`,
has no build tags, and builds with
`-buildvcs=false -trimpath -ldflags "-s -w"`. Disabling VCS stamping is
required because the Docker build context has no `.git` directory. The
reviewed binaries therefore contain no checkout-specific revision or time.
The consumer selects
`google.golang.org/grpc` v1.83.2. `go.mod` and `go.sum` are byte-pinned by the
collector as well as enforced by `-mod=readonly` and `go mod verify`.

The build runs `go list -mod=readonly -deps -json .` with the same `GOOS=linux`
and `GOARCH` as the binary. The bounded collector rejects load errors,
duplicate packages, missing module sums, source outside the main tree or
private module cache, symlinks, special files, changed files, incomplete root
license evidence, unknown license text and all count/byte-limit violations.

## Observed targets and text families

Local exact Go 1.26.7 collection produced:

| Target | Packages | Modules | Candidate files | Unclassified |
|---|---:|---:|---:|---:|
| `linux/amd64` | 921 | 142 | 193 | 0 |
| `linux/arm64` | 919 | 142 | 193 | 0 |

The module sets and candidate bytes are identical. The amd64 graph alone adds
`github.com/klauspost/compress/internal/cpuinfo` and the Go standard-library
vendored `x/sys/cpu` package. Those package differences do not add another Go
module or license text.

Observed unclassified license evidence: **none**.

The exact observed module-family counts are:

| Text family | Modules |
|---|---:|
| Apache-2.0 | 61 |
| BSD-2-Clause | 4 |
| BSD-3-Clause | 56 |
| CC0-1.0 | 1 |
| MIT | 50 |
| MPL-2.0 | 1 |

Counts can overlap when a module tree contains more than one license family.
The classifier requires multiple full-text markers rather than a title alone.
Three filename matches receive fixed digest-bound dispositions instead of a
license-family claim: Mergo test JSON, quic-go's retained logo/trademark policy,
and a retained Windows-only Wintun binary license. Go's BoringCrypto-derived
license is also retained as a separately identified toolchain text. These
inputs remain in the readable notice even though they do not describe another
compiled Linux license family.

`github.com/go-sql-driver/mysql` v1.9.3 is the sole compiled MPL-2.0 module. The
collector creates a normalized full-module `tar+gzip` archive: 50 entries and
443,891 uncompressed bytes. The canonical JSON member manifest binds every
path, type, normalized mode, byte count and content digest at SHA-256
`4a565566abcab4711cb605658718e7fa8b86c5b8d436a4b11387b6d11a607110`.
The local Python 3.14.7/zlib 1.2.12 run produced a 109,494-byte gzip stream at
SHA-256 `0732463671a1d0e270d518e2973cf68f28c3720940e921e992479823cb10ad8d`;
that compressed-stream hash is recorded by its manifest but is not used as a
cross-zlib source identity. The verifier accepts different compressor output
only when the single gzip stream expands to the exact canonical USTAR bytes;
trailing data and additional gzip streams fail. The package also records the
exact module sum and public module-proxy source URL. No other
corresponding-source requirement is inferred by the tool.

## Packaged evidence

Each architecture image receives only the target-specific result under
`/usr/local/share/covalent/caddy/`:

- `target-license-inventory.json`, binding the target graph and every candidate;
- `THIRD-PARTY-NOTICES.txt`, a deduplicated readable copy of every observed
  license/notice text plus the Go toolchain texts;
- `sources/0040-source.tar.gz`, the normalized MySQL source archive;
- `manifest.json`, binding those files and the exact Caddy binary digest.

The local output is about 600 KiB before container-layer compression: its exact
inventory and manifest sizes vary by architecture, while the readable notice is
365,690 bytes and the source archive is 109,494 bytes. The configured amd64
executable is 7,315,456 bytes smaller than the prior standard-module build.
The last tested amd64 image was still 518,144 bytes above its 128 MiB limit
after executable-only savings; only a complete final-image build can establish
whether the smaller evidence and dependency layers recover the remainder. This
document does not raise that limit.

The final image copies `/etc/ssl/certs/ca-certificates.crt` from the Go/Caddy
builder. The evidence therefore also verifies its actual owning package in the
builder APK database: `ca-certificates-bundle` 20260611-r0, origin
`ca-certificates`, declared `MPL-2.0 AND MIT`, aports commit
`6e30aafe4fa807ba70797509731f1e7d644dc8f3`, and exact bundle SHA-256
`b8d837841b88bfaa1a0fa827cbca8e2576418dd47c9fc4bb7f1f9d89c83111b9`
(179,359 bytes). The final-image contract extracts that runtime file and verifies
its exact bytes together with the complete package, path and target-architecture
record. The manifest leaves its source/license review explicitly open; the Caddy
collector does not substitute for the runtime Alpine review.

The collector prints one compact JSON summary in the normal build log with
target/package/module counts, family counts, empty unclassified evidence,
inventory/notice/binary hashes, source count/bytes and copied-CA identity. Full
texts and source stay in the image rather than being dumped into logs.
