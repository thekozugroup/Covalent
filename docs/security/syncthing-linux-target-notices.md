# Linux synchronized-folder target notices

Status: build integration implemented; exact `linux/amd64` and `linux/arm64`
container executions remain required before release review.

This process collects evidence. It does not classify licenses, approve a
release, or replace review of the resulting texts.

## Exact build relationship

Each architecture is built in its own pinned
`golang:1.26.7-alpine3.23` image. `build-linux-sync-engine.sh` first builds
Syncthing v2.1.3 from commit
`946e2b83a1f6c6ae119427c09e0a5802940b82ff` with `GOOS=linux`, the image's
exact `GOARCH`, `CGO_ENABLED=0`, `GOFLAGS=-mod=readonly`, and the upstream
`noupgrade` build tag. The builder verifies and applies Covalent's
`keep-local-deletions.patch`, SHA-256
`e58e7d133a388576a54cacc6a5a5094e6607c483c0daabac552de1a1854d92ac`,
only to the private source export. It then runs this exact target query:

```text
go list -tags noupgrade -deps -json ./cmd/syncthing
```

The source `go.mod` and `go.sum` must match their pinned SHA-256 values before
the build and remain unchanged afterward. The worker's `go version -m` output
is retained separately from the source graph.

`collect-go-target-license-inventory.py` rejects incomplete package loads,
unbound packages, source paths outside the exact source export or private
module cache, symlinks, special files, missing root license candidates, and
all configured entry, path, file, and byte bound violations. Replaced Go
modules retain both the requested module identity and the exact replacement
source identity.

`collect-sync-engine-notices.py` copies the byte-exact candidate files from
that inventory, the Go 1.26.7 distribution notices, the guardian source and
project license, and the embedded font's OFL text. It emits a machine-readable
manifest and a deterministic `THIRD-PARTY-NOTICES.txt`. The pinned Alpine
Python package is used only in the builder stage; it is absent from the
runtime image. The package version is recorded by the official Alpine package
index: <https://pkgs.alpinelinux.org/package/v3.23/main/x86_64/python3>.

## Installed evidence

Each architecture-specific runtime image contains:

- `/usr/local/share/covalent/sync-engine/THIRD-PARTY-NOTICES.txt`;
- `/usr/local/share/covalent/sync-engine/notices/manifest.json` and every copied
  source notice;
- `/usr/local/share/covalent/sync-engine/evidence/go-target-deps.ndjson`;
- `/usr/local/share/covalent/sync-engine/evidence/go-version.txt`;
- `/usr/local/share/covalent/sync-engine/evidence/target-license-inventory.json`;
- `/usr/local/share/covalent/sync-engine/PROVENANCE.txt` and package manifest hashes
  binding all of the above.

The raw dependency graph is capped at 16 MiB, its derived inventory at 16 MiB,
the binary metadata at 1 MiB, and the notice tree at 80 MiB. This slice does
not alter the runtime image budget; an unexpectedly large real target bundle
must fail the receiving branch's existing artifact gate.

## Release work still required

The hosted build must execute both target containers and record module counts,
notice counts and bytes, evidence hashes, final image sizes, and whether the
two target graphs differ. A reviewer must classify the exact collected texts,
confirm required attribution presentation, and confirm the MPL-2.0
corresponding-source offer for the shipped Syncthing source and changes. The
upstream source reference remains
<https://github.com/syncthing/syncthing/tree/946e2b83a1f6c6ae119427c09e0a5802940b82ff>.
The target notice manifest and combined recipient notice bind the patch digest
to the bundled modified-source archive.

Android already has an exact target inventory and generated proof bundle, but
the production APK has not yet proved installation or in-app notice access.
Its NDK/platform link material remains a separate review item. macOS needs its
own exact shipped target inventory, notice bundle installation, and notice UI
proof; Linux evidence must not be substituted for either platform.
