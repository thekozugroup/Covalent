# Receiving-copy isolation and remaining batch coupling

This draft improves receiving links without changing accounts, pairing identities,
policy files, or consent. It remains unready for release: real concurrent tests
still expose shared source-batch stalls and timing failures.

## Implemented

Completed inventories, copies and initial scans wake their existing service.
The independent health deadline stays active and retains a full interval after
a slow probe; completion traffic cannot postpone it. Cancellation still closes
the owned command lifeline. No service, thread pool or dependency is added.

Destination-only sessions can replace receiver configuration while unchanged
jobs retain their exact runtime, roots, grants and worker lease. Active job
folder configuration, peer binding and generation must remain identical. Changed
active authority and source/server configuration retain the existing stop
boundary. Initial metadata scans can be replaced without cancelling those copy
jobs. The appliance's restricted SAF/receiving-role checks remain in preparation.

An in-flight job owns its policy staging. Duplicate runtime preparation does
not misclassify that atomic write as damaged state; every new transfer still
loads and validates policy, and inactive staged records still refuse startup.
SAF reservations now unwind on preparation errors and count a repeated grant
once per runtime, while overlapping runtimes keep independent reservations.

The Android settings client builds one request/change ID and retries only the
explicit typed retryable503 busy response, with the existing bounded backoff.
Uncertain transport, malformed replies and other errors are never replayed.
The real-worker harness uses that same bounded contract for acceptance/settings;
its sixty-second file assertions and two-test concurrency are unchanged.

## Verification scope

316 host node tests, full all-target/all-feature clippy, foundation,223 actual
Android app JVM unit tests (including9 busy/replay/cancellation cases), and
Android26 ARM64 production node/JNI checks pass. No APK installation, full
native-linked package, physical-device or Google Photos claim follows from them.

The instrumented baseline delivered a remounted file at60876ms, after its
original60000ms deadline; its result stays failed. Original checkpoints reached
59990ms. Temporary production traces and post-failure observation are removed.

A focused final-source two-case concurrent run passes collection/remount,
including recovery after permission failure, but fails fresh online fanout after
the other destination stops. Its remount checkpoint is27594ms. The complete
seven-case run finishes with6 passed,1 failed (collection deletion barrier),
0 ignored and0 filtered. Its real fifteen-minute scheduled case passes. The
three-node case passes in that run; the earlier contrary witness remains an
unresolved source-fanout failure. Isolated passes do not resolve counterexamples. Exact results are retained privately
with sanitized source/log bindings in artifacts/evidence/sync-concurrency/.

## Why fanout remains blocked

The source owns one authoritative run per folder. Automatic admission skips an
active run, source inventory is prepared only in the Preparing phase, and the
whole run remains active while any destination is pending. A changed source
correctly fails the old inventory comparison at an online destination. If
another destination is offline, the source cannot admit a fresh inventory for
the online destination until that shared run ends.

Do not delete recovery state, increase the file deadline, force a pending peer
complete, or stop a possibly writing peer merely because its control connection
is unavailable. Those actions would hide the failure or risk original bytes.

## Next implementation boundary

Each destination needs independent progress with an immutable admitted inventory.
The existing wire already projects a signed run to one target. Investigate
bounded retained generations for disjoint pending destinations: a new generation
for an eligible destination must leave another destination's active generation,
proof, policy and SFTP access intact. At most one source inventory should be
preparing for a folder at a time. Reuse a source server only while its authorized
configuration is identical; a fresh inventory must remain a real full proof.

Audit targeted delivery/report lookup, per-share summaries, request generation
checks, restart reconciliation, expiry, membership/settings revocation and
backlog pruning together. Receiver commits must stay monotonic for that peer;
old pending reports must be acknowledged by a valid later generation. Do not
replace an inventory inside an active generation: old receivers can retain a
report and stop admitting work. Keep malformed state fail-closed and establish
explicit old-source/rollback behavior before writing new durable fields.

Required evidence: deterministic offline fanout with a source edit after the
first inventory; two simultaneous receiving links; independent policy/pause/
revocation changes during real copies; interrupted copies preserving unowned
bytes; restart/rollback and old-peer fixtures; the complete seven-case real
worker suite; then actual Pi/companion handoff. Container image scans and the
physical/cloud/gold gates remain independent.

The previous hosted API37 run passes86 baseline instrumentation tests but fails
its real SAF public Manual Run Now journey with generation0 on both nodes and
no pending request. This is distinct from the older emulator missing-service
failure. New bounded saved-request/button-state diagnostics compile locally;
they have not executed on a device. The same hosted revision now passes both
image/package contracts and fails high-severity go-jose matches in Caddy. Those
release gates remain enabled and separate from the Pi's restricted worker.
