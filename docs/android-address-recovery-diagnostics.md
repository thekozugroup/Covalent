# Intermittent Android address-change transfer

The final 0564034 app passes pairing, share acceptance and its first actual SAF
transfer. One retained journey then misses the existing transfer deadline after
its peer and worker ports change. Both paired identities and the authoritative
new address remain correct. That failed result is unchanged under
`artifacts/evidence/android-acceptance-busy/native-picker-readiness/`.

The journey now retains both endpoints' existing bounded recovery diagnostics
if that exact post-address transfer misses its deadline. It associates them
with the original request ID and records lifecycle, share/run phase, generation,
settings confirmation, destination outcomes and folder-health classifications.
The diagnostic helper does not emit tokens, folder paths, file contents or raw
worker errors. It performs status reads only after the original wait fails.

The request, timeout, expected file bytes, authentication, address proof and
cleanup remain unchanged. There is no mutation replay, retry input, permission
reset, data reset or device restart. This adds evidence for the next failure;
it does not itself fix the intermittent stall.

## Verification

Build instrumentation with `-PcovalentBuildSyncEngine=true`. An initial build
omitted that flag and inlined the unpackaged-engine constant into the test APK.
Its separate emulator result correctly fails before pairing. The corrected
build preserves the exact production debug APK and updates only instrumentation
on the same test device, without clearing data. Both build receipts and the
original failure are retained under
`artifacts/evidence/android-address-recovery/`.

The follow-up uses a new owned API 37 ARM64 emulator and synthetic SAF folders.
It verifies APK/helper bytes, source hashes, the same boot and framework process
identities, and the unchanged failed receipt. An earlier failed journey's
existing transfer-diagnostic log contains no retained events; that absence is
not a diagnosis. No physical phone, Pi, account or original media is changed.

The corrected journey passes in 437.554 seconds (435.768 reported by JUnit),
including actual manual/Unicode/address-change/restart transfers, lost-grant
isolation and repair, shared settings, pause/resume, deletion behavior and
removal. The app APK, boot, framework identities and all captured sources stay
unchanged. The earlier failed receipt is byte-identical. The new post-address
failure branch is not exercised on this successful run. The intermittent stall
therefore remains unresolved; this pass does not establish its cause or fix it.

The unchanged dedicated Interlink debug APK also passes a separate first-launch
and one permission deny/allow recovery check on that emulator. The app reaches
its unpaired native-ready screen with the logo and serif title, without pairing
or selecting media. This does not establish companion transfer, background or
large-font acceptance. The generic mobilecli Monkey launch first refuses the
emulator's missing physical system keys; its screenshot shows the launcher.
An explicit launch of the independently resolved manifest activity succeeds.
Both results remain separate. The observed unpaired screen exposes source
folder controls before pairing; progressive setup is the next UI change.

The final route gate and full local foundation suite pass. No additional JVM
rerun is claimed: the production app source and its previously verified 222-test
build remain unchanged in this diagnostic milestone.

Hosted source 0564034 CI39635433377 is kept running. The exact historical-client
gate correction e01dae1 is already published separately; this diagnostic change
also stays separate until that original run is terminal. Private appliance CI
at 932eb40 cannot start because GitHub reports an account billing/spending limit.
Release and physical-device acceptance remain open.
