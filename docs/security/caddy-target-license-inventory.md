# Caddy compiled-target license and source evidence

This is an engineering inventory, not legal advice or release approval. It
records the exact texts observed in the pinned Caddy consumer's compiled Linux
graphs and identifies separately retained source material.

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
