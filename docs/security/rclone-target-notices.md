# Rclone target notices

The worker is built from the repository's `packaging/rclone` module, which pins
upstream rclone v1.75.1 and its Go module checksums. Upstream transfer code is
unmodified. The bundled worker includes only selected backends and commands.

Each platform builder runs `go list -deps -json .` with the same target and
build settings as its executable. `collect-go-target-license-inventory.py`
records the selected dependency files. `collect-sync-engine-notices.py` checks
their bytes and builds the readable notice bundle and manifest. Packages retain
the rclone MIT license, Covalent's license, Go's notices, and the selected
dependencies' notices. Source records are retained where the collector requires
them. Adding a dependency requires new target evidence.

The macOS build uses Go 1.26.7, arm64, and CGO disabled. Android uses Go 1.26.7,
NDK 27.1.12297006, API 26 PIE executables, and 16 KiB segment alignment on both
arm64 and x86_64. Linux builders use the exact target architecture in the pinned
Go container and CGO disabled. All builds use the pinned module graph and
disable VCS-dependent build metadata.

The package scripts bind executable and notice digests to their manifests.
Android additionally retains final-link provenance. Successful collector or
cross-build tests alone do not establish a working final application or Docker
image; those remain separate release checks in the completion ledger.

The former Syncthing inventories describe earlier builds and remain available
in Git history. They are not evidence for rclone packages.
