# Core reliability audit — 2026-09-07

Scope: `covalent-core` and `covalent-node` crash recovery, durable writes, restore safety, and replica streaming behavior. This is working-tree evidence, not final-release certification.

The backup and restore paths journal encrypted chunk batches before publication, sync checkpoints before completion, and publish snapshot metadata after referenced local chunks are durable. Restore files are fully written, metadata-applied, and synced before a durable `Prepared` WAL record; renames are followed by parent-directory sync. Startup reconciles local/provider journals, and an exclusive process lock protects engine state.

Recovery authenticates every capsule and decrypted manifest before comparing candidates. A bad unsigned capsule cannot block recovery by claiming a newer timestamp. Owner-signed but unusable newer evidence does block an older candidate. Failed providers are isolated and exact verified source providers remain visible with `newerSnapshotMayExist`; an incomplete catalog listing is never described as complete.

Production local-store and QUIC providers now stream capsules. The importer retains only the latest encrypted capsule per backup, plus compact digests that still detect conflicting authenticated duplicates in discarded history. A 384 MiB live serialized-data budget charges retained capsules, one reserved incoming capsule, and index overhead before fetching a body; a separate one-million-catalog limit bounds traversal. Local reads reject symlinks and growth beyond the reservation. QUIC uses bounded descriptor pages and checked segments, identities, sizes, and digests. The trait's default visitor returns unsupported rather than materializing an aggregate legacy list. The byte budget is **not an RSS guarantee**: decoding, signature verification, and one decrypted capsule add working memory. Large-catalog memory measurements on final native artifacts remain required.

Recovery writes durable snapshot watermarks before publishing keys or snapshot metadata. A crash or cancellation before the remembered-backup configuration is published therefore cannot allow an older provider copy to become the latest snapshot on retry. The same or a newer authenticated copy can complete the interrupted import. Configuration writes are checked against the exact pretty-JSON read limit without allocating a second serialized buffer.

The recovered identity and configuration publish a durable runtime-handoff marker together. A normal restart repairs signed provider connections and pending recovery status before clearing that marker, including a crash between core bootstrap and runtime setup. Startup and API retries share cancellation state; shutdown cancels recovery before waiting for HTTP handlers to drain. Engine and operation-lock acquisition is cancellation-aware, and network cancellation drops the in-flight request.

Authenticated, explicitly confirmed kit-export, status, and retry endpoints connect this engine behavior to the clients. Exported material is never included in recovery status. The standalone `recover` command accepts private raw recovery/code files or a recovery-only `CVSEC003` inherited pipe; V1/V2 ordinary startup remains compatible. Durable status has bounded counts and bytes, rejects contradictory states, and falls back to a compact blocked report when evidence cannot fit. A verified capsule received before a later page fails may appear as recovered while that provider remains absent from the completed-listing set.

Validation in this working session includes:

- Complete Rust workspace run: 307 tests passed across 22 suites, zero failed or ignored, including the final status-validation and shutdown regressions.
- Eighteen focused core recovery tests, including newest-first and oldest-first streaming within a budget smaller than the complete history; conflicting discarded duplicates; cancellation; restart before/after snapshot publication; and configuration-size limits.
- Eight recovery-state tests covering serialized publication, bounded evidence, partial streaming results, and cancellation while another operation holds the lock.
- A live HTTP/QUIC owner-loss test: remove the original owner's entire state root, bootstrap from the exported kit, observe the offline-provider warning, reconnect the original signed endpoint, and restore exact file bytes and an empty directory using provider data.
- Two runtime restart/shutdown tests: normal startup repairs the handoff crash window, and automatic or explicit recovery against a silent signed endpoint stops and joins within three seconds after an actual outgoing QUIC datagram. Restart proves the engine lock was released.
- Strict workspace Clippy with warnings denied; final-revision formatting, contracts, native UI, and artifact checks must still be repeated at the release checkpoint.

The tests simulate interrupted durable boundaries and replica faults. They do not reproduce physical power loss on every target filesystem, prove Android memory behavior, or replace the Atlas installation gate.
