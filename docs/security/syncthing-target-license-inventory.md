# Syncthing target license and notice evidence

This is an engineering inventory, not legal advice or release approval. It
records the exact texts observed for the pinned engine and names the remaining
distribution decisions.

## Evidence reviewed

- Syncthing `v2.1.3`, commit
  [`946e2b83a1f6c6ae119427c09e0a5802940b82ff`](https://github.com/syncthing/syncthing/tree/946e2b83a1f6c6ae119427c09e0a5802940b82ff).
- Successful proof run `34241597202`, commit `5c157f37`, supply-chain artifact
  `10062298950`, artifact SHA-256
  `89f6a221e736215b30314653d99b8359ab92af6e2367a969da02622ee8a57a2f`.
- Artifact inventory: 60 modules on each Android ABI, 465 arm64 packages, 467
  amd64 packages, 95 candidate files (79 license, 16 notice), 274,030 bytes,
  and no missing root-license evidence.
- Go `1.26.7` source archive from the
  [official download service](https://go.dev/dl/), 34,150,794 bytes, SHA-256
  `0ed24eac755105085b89fe9cabc2742b91a0ad7b94b59d3ad364918ebc8956ad`.
- Shared guardian source SHA-256
  `c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579`;
  the project MIT license SHA-256 is
  `ff9bc316792502655bcae384b8e5a4604da93385fda00552b77d85c334b94acd`.
- Full OFL-1.1 text from the
  [SIL Open Font License site](https://openfontlicense.org/open-font-license-official-text/),
  SHA-256 `1d361a8f8e8ce6e68457dcd93fb56e162e6baa3bbb7e7573a290d44399f6b57e`.

The proof collector scans candidate filenames through each compiled module,
not only directories of imported packages. This deliberately over-collects
some material. For example, the quic-go brand-assets policy and Syncthing relay
tool licenses appear even when those assets or commands are not in the worker.
The bundle keeps every observed candidate rather than guessing it is safe to
drop one. Syncthing's generated GUI is embedded in the worker, so its vendored
GUI license files remain material.

## Text families observed

These labels describe the exact license texts. Mixed rows mean compiled
packages or embedded assets within that module are covered by more than one
text. They are not a compatibility opinion.

| Text family | Modules | Count |
|---|---|---:|
| MPL-2.0 | `github.com/AudriusButkevicius/recli`, `github.com/hashicorp/errwrap`, `github.com/hashicorp/go-multierror` | 3 |
| MPL-2.0 and BSD-3-Clause | `github.com/hashicorp/golang-lru/v2` (compiled `simplelru`) | 1 |
| MPL-2.0 with embedded MIT and OFL-1.1 GUI assets | `github.com/syncthing/syncthing` | 1 |
| MIT | `github.com/Azure/go-ntlmssp`, `github.com/alecthomas/kong`, `github.com/beorn7/perks`, `github.com/calmh/incontainer`, `github.com/calmh/xdr`, `github.com/cespare/xxhash/v2`, `github.com/cpuguy83/go-md2man/v2`, `github.com/go-asn1-ber/asn1-ber`, `github.com/go-ldap/ldap/v3`, `github.com/gobwas/glob`, `github.com/jmoiron/sqlx`, `github.com/kballard/go-shellquote`, `github.com/mattn/go-sqlite3`, `github.com/miscreant/miscreant.go`, `github.com/posener/complete`, `github.com/quic-go/quic-go`, `github.com/riywo/loginshell`, `github.com/stretchr/objx`, `github.com/stretchr/testify`, `github.com/syncthing/notify`, `github.com/thejerf/suture/v4`, `github.com/urfave/cli`, `github.com/willabides/kongplete` | 23 |
| Apache-2.0 | `github.com/ccding/go-stun`, `github.com/jackpal/go-nat-pmp`, `github.com/prometheus/client_model`, `github.com/prometheus/common`, `github.com/prometheus/procfs`, `github.com/tklauser/numcpus` | 6 |
| Apache-2.0 and BSD-3-Clause | `github.com/prometheus/client_golang` (compiled gddo code), `github.com/vitrun/qart` | 2 |
| MIT and Apache-2.0 | `gopkg.in/yaml.v3` | 1 |
| BSD-3-Clause | `github.com/gofrs/flock`, `github.com/golang/snappy`, `github.com/google/uuid`, `github.com/jackpal/gateway`, `github.com/julienschmidt/httprouter`, `github.com/munnerz/goautoneg`, `github.com/pierrec/lz4/v4`, `github.com/shirou/gopsutil/v4`, `github.com/tklauser/go-sysconf`, `github.com/wlynxg/anet`, `golang.org/x/crypto`, `golang.org/x/exp`, `golang.org/x/net`, `golang.org/x/sys`, `golang.org/x/text`, `golang.org/x/time`, `google.golang.org/protobuf` | 17 |
| BSD-2-Clause | `github.com/pkg/errors`, `github.com/pmezard/go-difflib`, `github.com/rcrowley/go-metrics`, `github.com/russross/blackfriday/v2`, `github.com/syndtr/goleveldb` | 5 |
| ISC | `github.com/davecgh/go-spew` | 1 |

No GPL, LGPL, or AGPL license text appeared in the 60-module Android target
inventory. That statement is scoped to this exact report and candidate scan.

## Reproducible bundle

`scripts/collect-sync-engine-notices.py` consumes the passed target inventory,
the exact source export and module cache used by `go list`, a Go 1.26.7 source
root, the guardian source, and the project license. It rejects missing,
symlinked, changed, oversized, unpinned, or incomplete input. It copies each
candidate under a stable numbered module directory and writes its manifest
last. It never downloads or installs a tool.

For the proof-13 Android inventory it produced:

- 60 module records and all 95 observed module candidates;
- Go root `LICENSE` and `PATENTS`;
- matching license/patent files for compiled vendored `x/crypto`, `x/net`,
  `x/sys`, and `x/text` standard-library packages;
- Go's conditional `crypto/internal/boring/LICENSE`, because that package name
  appears in the graph, without claiming `GOEXPERIMENT=boringcrypto` was used;
- the complete OFL-1.1 text in addition to Fork Awesome's short upstream
  license reference;
- the guardian source and Covalent MIT license.

The result is 109 payload files and 321,396 bytes, plus `manifest.json`. Its
manifest SHA-256 is
`194ab907dccf3da2bb94e70d942887b14571ebb4763523d851098b3012d579ea`.

The collector invocation is explicit about every source of bytes. Source,
module-cache, and Go roots must be absolute canonical directories, and the
output must not exist:

```sh
python3 scripts/collect-sync-engine-notices.py \
  --inventory "$TARGET_LICENSE_INVENTORY" \
  --source-root "$PINNED_SYNCTHING_EXPORT" \
  --module-cache "$PRIVATE_GOMODCACHE" \
  --go-root "$GO_1_26_7_SOURCE_ROOT" \
  --guardian-source packaging/sync-engine/engine-guardian.c \
  --project-license LICENSE \
  --ofl-license docs/licenses/sync-engine/OFL-1.1.txt \
  --output "$NEW_NOTICE_BUNDLE"
```

CI should pass absolute paths for the repository-owned inputs as well. A second
`--module-cache` can be supplied when an exact target collection uses more than
one private cache.

The build must retain the target inventory and notice-bundle manifest together.
Do not regenerate the inventory from `go list -m all`: that includes modules
which are not compiled and can also miss target-specific package distinctions.

## Release gaps

1. Generate a separate `GOOS=linux`, `CGO_ENABLED=0` inventory for both Linux
   architectures and build its notice bundle. Android's graph is not a valid
   substitute: it contains the CGO SQLite implementation, while other host
   builds have already shown additional pure-Go SQLite modules.
2. Install the complete generated bundle in the shipped image/APK and expose a
   readable notices route or screen. The two-file Syncthing license/AUTHORS
   placeholder alone is insufficient for the observed dependency and embedded
   GUI texts.
3. Preserve Syncthing's MPL-2.0 text and publish the exact corresponding source
   and any modified MPL-covered files by a stable release URL. The source URL
   and modification statement need release-owner review.
4. Review Android NDK link evidence separately. Android platform shared-library
   names do not themselves mean those libraries are redistributed, but any
   statically linked compiler runtime or copied NDK material must appear in the
   APK notice inventory.
5. Confirm that the official OFL text plus Fork Awesome's upstream copyright
   reference is displayed with the shipped font. Confirm whether quic-go's
   uncompiled brand-assets policy can be omitted only after package-to-file
   provenance proves no such asset enters the binary.
6. Treat the grouped text-family review as factual evidence. A release owner or
   counsel still decides the final notice presentation and license
   compatibility.
