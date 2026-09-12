# Alpine runtime source and notices

Covalent's Docker image includes Alpine packages in addition to the separately
built Rust node, Caddy and Syncthing worker. The image's MIT label describes
Covalent; it does not relicense those dependencies.

`packaging/docker/alpine/runtime-source-lock.json` records the exact sixteen
installed packages, their ten source origins, architecture, version, declared
license and base aports provenance. It applies the two pinned OpenSSL 3.5.8-r0
updates and the three Covalent-local BusyBox 1.37.0-r1000 packages to the pinned
Alpine 3.23.5 base. Schema 2 declares those three local packages explicitly:
their `installedDatabaseCommit` is null because their actual APK records omit
`c`, and only `busybox-binsh` overrides the target architecture with `noarch`.
The other thirteen package identities retain their upstream requirements.
An empty `c:` value does not equal an absent field. Notices distinguish the
absent package commit from the recorded original Alpine recipe commit.
The installed package database from the actual
runtime stage must match this list completely. An added, removed or changed
package fails collection instead of inheriting an old classification.

The CA file copied from the Go/Caddy builder has separate provenance. Both
architecture manifests of the pinned Go 1.26.7 Alpine image contain
`ca-certificates-bundle 20260611-r0`, origin `ca-certificates`, aports commit
`6e30aafe4fa807ba70797509731f1e7d644dc8f3`. The copied CA file is 179,359 bytes,
SHA-256 `b8d837841b88bfaa1a0fa827cbca8e2576418dd47c9fc4bb7f1f9d89c83111b9`.
Collection checks the actual builder's installed record and the copied file,
not only the final Alpine package record.

## Source selection

The reviewed lock contains 161 distinct inputs: complete files from the ten
exact aports recipe directories, upstream source archives, and the netbase
inputs declared by the baselayout recipe. Each input has an exact HTTPS URL,
size and SHA-256. Recipe entries also record their Git blob identity and original mode. Each origin
has an independently pinned Git tree SHA-1 and complete entry list. The collector
recomputes that tree and requires exact equality with the locked recipe files,
including install/trigger siblings that are not in APKBUILD source checksums. Files named
in an APKBUILD checksum section retain its SHA-512. The collector independently
requires that every declared APKBUILD checksum is represented; it never runs a
recipe or shell fragment to interpret this metadata.

The final `runtime-source.tar.gz` contains 161 inputs across nine origins:

- `alpine-base`, `alpine-baselayout` and `alpine-keys`;
- `apk-tools`, `busybox` and `ca-certificates`;
- `musl`, `pax-utils` and `zlib`.

This includes every GPL/MPL origin plus the small permissive components. Each
origin directory contains its complete recorded recipe directory, patches,
configuration and install/trigger files, together with original upstream
archives. The archive includes a source manifest and build-environment notes.
It retains upstream copyright and license notices, executable modes, and the
one upstream post-upgrade symlink to its included regular post-install sibling.
Arbitrary, external and cyclic symlink targets are rejected. The BusyBox local
override retains all 79 unchanged recipe siblings in `busybox/aport/`, replaces
its APKBUILD and adds the reviewed wget patch. The original APKBUILD, exact
recipe diff and provenance remain at the BusyBox root beside the upstream
tarball, which is included once. Four additional local inputs carry exact
hashes and bounds. The collector verifies both complete Git trees, the exact
base-to-local recipe diff, versions and every checksum declared by the modified
APKBUILD. It does not execute that recipe. The remaining origins are unchanged.

OpenSSL's 53 MB source archive is verified while collecting its exact Apache
license and AUTHORS file. It is not added to the runtime image. The inventory
retains its exact source URL, version, hashes and recipe commit. Its build-only
Perl tooling is not described as a shipped runtime dependency.

The CA source includes the original `certdata.txt`, its MPL notice, and the
build scripts. The readable bundle also retains the complete `c_rehash.c` MIT
copyright notice and `mk-ca-bundle.pl` curl notice. The canonical supplemental
[MPL terms](https://www.mozilla.org/en-US/MPL/2.0/),
[curl terms](https://curl.se/docs/copyright.html), and
[MIT terms](https://opensource.org/license/mit) are pinned separately with
source URLs and content hashes. Generic MIT terms do not invent copyright
holders; the component sources retain their actual notices.

## Collection and verification

`scripts/collect-alpine-runtime-evidence.py` runs in a build-only Python stage.
The final image receives only these three outputs:

- `THIRD-PARTY-NOTICES.txt`: package identities, source locations and full texts;
- `inventory.json`: exact metadata, source/input and output digests;
- `runtime-source.tar.gz`: deterministic corresponding source.

The collector verifies input files through held directory descriptors, rejecting
symlink traversal even if a parent path changes between checks. It rejects duplicate or
incomplete metadata, bounds downloads and archive inspection, and requires new
output. Archive inspection reads required regular members without extracting
paths to disk. Downloads use exact reviewed HTTPS locations and reject
redirects. Source and notice bounds are independent of the container size gate.
The lock's declared byte limits must exactly match the collector's enforced
limits. Output creation also uses held directory descriptors and exclusive
creation. A replaced parent cannot redirect writes, and existing output is
never replaced. Consumers must wait for the command to succeed; failed writes
are cleaned from the owned output directory. The build workspace must not have
other concurrent writers.
No Python interpreter or source-download cache is copied to the final image.

The Docker `alpine-evidence` stage receives `/lib/apk/db/installed` from the
configured `runtime-patched` stage. It separately receives the original Caddy
builder's installed database and CA file, before its evidence stage adds
Python. The final stage copies only the three generated evidence files.
`check-container-contract.sh` uses one temporary, unstarted container to extract
the final image's Caddy and Alpine evidence, installed package database and CA
file. Both independent verifiers must pass before the contract succeeds. The
container and extracted files are removed on success or failure.

CI logs bounded package rows and inventory, notice, source and CA hashes.
Those records identify the actual package database and copied CA evidence.
Image pins are explicitly labelled declared source selections, with validated
digest syntax; they are not presented as observed OCI identity. Final image
packaging and image identity need their separate container contract checks.

Twenty-three adversarial offline tests cover package/version/architecture drift,
missing or duplicate identity fields, parent-directory replacement during an
open, declared image-pin syntax, complete Git recipe trees, CA provenance, altered sources, omitted
GPL/MPL source, incomplete checksum coverage, unsafe paths/URLs, duplicate or
symlinked archive notices, FIFOs, byte bounds and preservation of existing
output. They also cover output-parent replacement, cleanup after a failed write,
and invalid declared bounds. A synthetic fixture proves deterministic byte-for-byte output.
Local-override fixtures cover absent versus empty commits, unchanged upstream
commit requirements, the shell package's architecture, exact transformed
recipe/source bytes and rejection of a mismatched recipe tree or diff.

The separate `verify-alpine-runtime-evidence.py` checks files extracted from the
final image. It compares the actual installed package database and CA file,
the complete readable notice text, the recorded source classification, and
every corresponding-source member against the reviewed locks. It verifies
regular file content, Git object identity, exact recipe symlink targets and
canonical archive metadata without extracting source files to disk. The entire
uncompressed USTAR encoding must match the verified members; hidden data after
the tar end marker, duplicate gzip streams and compressed trailers fail.
Different gzip compression bytes are accepted only when they encode the same
reviewed source and match the actual inventory's compressed-file digest.
Twelve focused verifier tests cover these conditions, including self-consistent
tampering of the package's own inventory and changed local-package provenance.

On September 12, schema 2 collection and package verification pass with all 161
upstream inputs and four local inputs. The arm64 collection uses the actual raw
APK database from the isolated Atmos `runtime-patched` build; amd64 uses an
explicit candidate database. The source archive is 6,032,198 bytes, SHA-256
`092d69bb7077543cde9de8d0bd52a12221e403a866848cd2adda60abf326eda1`.
These results do not establish either final Docker image, native amd64 package
execution, vulnerability classification or final image-size acceptance.

The initial real-source offline run verified all 161 inputs and both pinned
architecture package sets. Its runtime package databases were explicit
candidate fixtures constructed from the verified base and exact OpenSSL
package metadata. They are not claimed as final image executions. The source
archive is 6,019,693 bytes, SHA-256
`53eaf54b4a9a2367aa1dd879bfa125e852f9b2a8f688eb3b1f7b2a4d2448267a`.
The three outputs total 6,276,267 bytes for amd64 and 6,276,300 for arm64.
Final Docker execution must bind its own output to the exact image and measure
its actual size before acceptance.

## Security status

Source and license completeness do not establish vulnerability remediation.
Checkpoint 39 still reports three Medium package findings for
[CVE-2025-60876](https://security.alpinelinux.org/vuln/CVE-2025-60876) in BusyBox
1.37.0-r30, busybox-binsh and ssl_client. Alpine 3.23 currently has no evidenced
fixed APK. A separate backport needs explicit modified-source provenance and
runtime validation before this lock or a remediation claim can change.
