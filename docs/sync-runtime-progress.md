# Independent continuous destinations — draft

An offline destination previously held the source's one folder batch open. New
photos could then fail an old inventory comparison at an online destination,
with no new inventory admitted until the offline peer finished or expired.
Continuous links now admit a later generation for eligible terminal destinations
while pending destinations keep their exact original generation and proof.

## Authority and bounds

The source journal retains disjoint destination sets across generations. Each
peer occurs once, at most512 destinations total; superseded terminal entries and
empty old generations are pruned. The newest generation remains the owner's
admission counter. Only that newest generation can be Preparing, so one real
full source inventory runs per folder. A pending writer is never superseded.
Its deadline, proof and completion policy remain unchanged.

Commits select the peer's own generation; reports select that same destination's
state. Old reports cannot finish a newer generation. Retained generations share
normal expiry, restart interruption, membership removal and settings cancellation.
Duplicate peers, mismatched map/generation keys, retained Preparing states,
receiver-owned retained state, unknown members and over-limit records fail closed.

The existing status shape exposes the newest admission generation and all current
destination results. Its aggregate phase cannot claim success while another
peer remains active or failed; active timestamps use the earliest active batch.
No new control or wire fields are sent to receivers.

Manual and scheduled admission retain whole-batch behavior. Explicit settings
changes cancel every affected generation. If several generations were retained,
that cancellation publishes one common cancelled generation and drops retained
state. This lets existing manual clients resume their normal generation checks;
it does not fabricate a success or replace an active admitted inventory.

## Source server and receiving copies

Generation-only updates reuse a source server only when its complete authorized
settings remain identical and every folder is a source. Root, membership, key,
route, policy, role and condition changes retain their normal validation and
stop boundary. Every admitted generation still obtains a fresh complete inventory.
Mixed-role/legacy configurations keep their existing restart path.

The earlier receiver isolation remains: unchanged copy jobs keep their exact
runtime/root/SAF/worker leases during unrelated receiver changes; active authority
changes still stop and reap. Completion wakes do not starve independent health
probes. New transfers strictly validate policy; an active job owns its atomic
staging. Grant reservations unwind and count correctly across overlapping owners.
Android settings retries only the explicit typed retryable busy refusal using
the exact original request; uncertain requests are not replayed.

## Persistent compatibility and rollback

The optional source-only `retained` map defaults empty, so existing journals load
without rewriting or erasing them. It is absent when empty. Receiver v1 run
commits keep their original strict shape. Old source code intentionally refuses
nonempty retained state through its unknown-field check. Never strip the map to
force rollback: a writer or its result could be forgotten.

A forward-version source must first settle or explicitly cancel the retained
work, persist the ordinary single-generation shape, and stop before an older
source can be considered. Fixtures establish serialization/refusal behavior;
this is not signed device rollback, physical boot or migration acceptance. The
restricted Pi worker remains receive-only and never creates source retained runs.

## Verification

The exact new state-machine test and real-worker offline-fanout test both fail
on prior source5747776 at the intended admission/fresh-file boundary. The baseline
real test reaps all nodes and removes its synthetic root even on failure. Source
backups are restored and hashed before candidate checks.

318 complete host node tests, denied-warning all-target/all-feature workspace
clippy, and foundation pass. State fixtures cover repeated bounded generations,
late exact-proof reports, replay, reopen, expiry, interruption, settings changes,
legacy parsing/refusal, malformed state and member removal. One fixture initially
assumed that a share's Pause meant member-only pause; it correctly cancels shared
link settings. The fixture now uses member removal, preserving existing semantics.
The earlier317/1 result remains recorded, not relabeled as a pass.

The focused real worker test delivers two post-inventory files in17035ms and
17240ms while the other peer remains offline. The source server PID receipt
stays unchanged across those generations. After source restart, the returning
peer catches up in16870ms, with source and unrelated destination originals
verified intact. This focused run precedes the final aggregate-summary fix.
The final full eight-case concurrent worker suite is running, with the original
sixty-second file assertions and two-test concurrency, including its real
fifteen-minute scheduled run. No filtered or serial pass substitutes for it.
Android26 ARM64 production node/JNI checking passes. Actual appliance adaptation checking remains pending here.

## Independent release gates

Hosted CI41234049724 on5747776 finishes with six of seven real-worker tests
passing; online fanout fails. Both image/package contracts pass and strict scans
refuse High go-josev3/v4 matches in Caddy (GHSA-mgcc-cww5-fp2q). No exemption.
Its API37 run fails at emulator startup with a missing Android settings service,
before the new admission diagnostics execute. An older API37 run passes86 base
tests but fails the real SAF public Manual Run Now journey at generation0; that
application issue remains unproven. These failures are separate.

No phone/Pi installation, account/pairing/media mutation, full linked signed
bundle, merge, release or gold image is claimed. Physical sync/import/camera,
roaming, cloud-confirmed cleanup and update/rollback acceptance remain required.
