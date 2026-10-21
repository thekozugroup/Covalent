# Android folder acceptance during contention

An incoming share could fail during setup with “Folder sync is busy” even though
its peer and selected folder were valid. The node deliberately uses a nonblocking
sharing lock. Android previously returned its first temporary refusal directly
to the user. Hosted run 39622053150 at 69ff050 failed at this point before the
permission-recovery phase.

`acceptFolder` now retries only a complete HTTP 503, protocol-1
`folder_sync_busy` response marked retryable. Its request keeps the exact base
URL, token, offer ID and selected destination. There are at most six attempts,
with waits of 100, 200, 400, 800 and 1600 milliseconds. Persistent contention
returns the last refusal. Cancellation stops further attempts and preserves the
thread's interrupt flag.

The parser records whether all retry-contract fields have their explicit JSON
types. Legacy generic error defaults and coerced strings never authorize replay.

This relies on this endpoint's current server contract: `FolderSyncService.accept`
can return `Busy` when it cannot acquire the sharing lock, before the acceptance
journal mutation. A committed acceptance returns a mutation response even if
worker reconciliation needs attention. The journal also treats repeated exact
acceptance of the same captured root as idempotent. No server lock behavior or
permission check is relaxed.

Timeouts, lost responses, invalid acknowledgements, unknown protocols, other
HTTP errors and nonretryable responses are never replayed by this helper.
Other sharing operations keep their existing behavior. The Android grant journal
still saves the chosen destination before sending the acceptance.

## Verification

The original 69ff050 client fails a new deterministic HTTP regression after its
first busy reply. The updated client survives two busy replies, then accepts;
all three requests contain the same authenticated destination selection. Eight
additional helper cases cover immediate success, transient/persistent contention,
strict response eligibility, uncertain responses and cancellation. A separate
HTTP regression rejects missing or coerced contract fields. All 222 JVM tests
pass. Both Covalent and the dedicated Interlink debug APK compile and pass
lint with the real packaged native workers. These are test-signed artifacts.

Native-helper QA now accepts an explicit `--abi arm64-v8a` and verifies the
installer's `lib/arm64` directory against the exact APK's ARM64 manifests and
bytes. The existing default remains x86_64. Seven checks pass on macOS and root
Linux, including refusal to substitute valid x86_64 bytes for damaged ARM64
bytes. This changes QA only.

The fresh `Interlink_Acceptance_API37` emulator on port 5606 retains the initial
failed Intel-only helper preflight. An explicit continuation verifies the same
already installed APKs and ARM64 helpers without reinstalling. Its first real
SAF journey is recorded separately under
`artifacts/evidence/android-acceptance-busy/native-continuation/`.
The earlier source passes in 438.849 seconds (437.175 seconds reported by
JUnit), with the same boot and framework PID/start-time identities and unchanged sources. It checks
real system folder selection, native transfers and Unicode filenames, peer
address changes, restart, lost-grant isolation and repair, settings, pause/resume,
confirmed synthetic deletion behavior and removal. The failed preflight stays
unchanged. This integrated journey does not establish how often real lock
contention occurred; the deterministic HTTP regression proves that behavior.

Final review then tightens the explicit-response requirement. The earlier
builds, reports and source snapshots stay under `before-explicit-contract/`;
the strict-source build passes separately. Its `native-strict/` attempt on the
already paired emulator fails because that fixture hides the first-time pairing
form. No pairing data is cleared.

A separate fresh emulator, `Interlink_Strict_API37` on port 5608, exposes a test
readiness race before the first folder selection: node readiness precedes the
asynchronous status screen. Its `native-final-fresh/` failure remains separate.
The test now observes the exact chooser until it is displayed and enabled,
within its existing deadline, before the one click. It repeats no picker input.
The production APK stays unchanged; only the instrumentation APK is updated
for the separate `native-picker-readiness/` journey. That final-source journey
runs for 200.369 seconds (JUnit 198.698 seconds). Pairing, acceptance, the first
manual transfer and identity-preserving peer/worker port changes pass. The
changed address becomes authoritative, but the next requested file does not
arrive before its existing deadline. The validator correctly rejects the
journey despite ADB's zero exit code. Its cause remains unknown. Later restart,
grant-repair, settings and deletion stages are not reached on the final source.
Boot/framework identities, production APK, sources and the original failed
receipt remain unchanged. No earlier result is relabeled as final-source
acceptance; full native and release gates remain closed.

Both debug APKs also pass package budgets, native-byte/manifest checks,
16-KiB alignment and signature verification. Their debug signing identity is
not production signing. Source/evidence bindings are in
`artifacts/evidence/android-acceptance-busy/manifest.json`.

The initial wrong module name and host Go-version build failures remain in
separate logs. The successful builds use pinned Go 1.26.10 and Android NDK
27.1.12297006. The chooser-test rebuild stops separately after macOS offloads
`src/debug/elf/file.go` from the pinned toolchain: sampling observes the same
Go-list process blocked in read, and the file has zero allocated blocks and the
`dataless` flag. Cloud download requests do not restore it. The exact owned
Go-list child is terminated only after a local archive restores the same Go
executable outside cloud storage. The terminal failed build log, archive SHA,
source-file hash and restoration receipt remain separate. The new build uses
that verified local toolchain; no live test is restarted for an observation
timeout.

This work does not change installed physical devices, account
state, original photos, the Pi image, production signing or release gates.
