# Sandboxed packaged-worker gate — 2026-10-21

The fresh 9437714 macOS archive builds in258.406s. Its production build inputs
remain unchanged; the only input edited while archiving is foundation's test
list. The exact archive is signed for local/CI hardened-runtime testing. This
is ad-hoc QA signing, not Developer ID distribution or notarization.

The same archived worker traps with SIGTRAP from an ordinary Python parent
(0.469s, no output) and runs its version command successfully under the native
sandboxed parent. The parent observes a real outside-file read denial and
writable private temporary storage. A separately signed, owned parent without
App Sandbox is rejected by the guard with exit65 before any worker launch.
No production worker is re-signed or given broader permissions.

The first full sandboxed scenario fails in135.83s test /136.841s wrapper after
all three acknowledged transfers and both endpoint moves. An exact completed
request replay immediately after reopening receives HTTP503 folder_sync_busy.
The service intentionally returns that retryable response while its mutation
lock is held. The existing test helper handles it for initial submission but
not replay. This failure, its original source binding, and confirmed cleanup
are retained; the raw failed log remains ignored with its digest recorded.

The updated helper submits the same request bytes for both initial calls and
replays, retries only the explicit busy response, and keeps the original
60-second operation /240-second scenario limits. The complete gate passes in
136.43s test /137.552s native-parent execution. Identities, generation3, manual
cadence, exact request replays, originals and receiver deletions survive restart.
One reopened address replay observes a busy response; both completed-run
replays return without one. The latter retry branch is therefore not exercised
by this passing run. Worker and fixture cleanup is confirmed. The archive's
complete file inventory, copied workers and original Cargo binary stay intact.

Nine offline Cargo-artifact negative/selection fixtures pass. Final foundation
passes in46.846s; Clippy with warnings denied passes in1.433s. Formatting and
whitespace checks pass. The manifest binds current source and retained evidence;
large build logs, binaries and synthetic app bundles remain ignored. The new CI
step retains only its receipt and logs, on success or failure.

Hosted9437714 job119440676123 reproduces the old first-batch failure in75.39s
before either address moves, with preparing/initialScan and no observed worker
health. Its full connector-decoded log is retained locally; the committed
summary binds that log and its failure lines. These local controls support the
launch-context correction. Hosted acceptance of the revised gate remains open.

No Android APK, production runtime, Pi image, installed app, account, pairing
store or original media is modified. This gate does not resolve the older
intermittent Android post-address timeout, hosted Android emulator abort,
physical Pi/phone checks, or final release/image acceptance.
