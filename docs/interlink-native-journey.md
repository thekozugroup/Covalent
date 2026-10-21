# Native journey under the dedicated companion identity

The real SAF folder-sync journey now builds for the separate Interlink package.
Its phone screen uses `photoAppliance = true` with Material Expressive theme and
motion. It pairs through the UI before revealing source selection, stops its
owned synthetic peer, selects both system folders while that peer is offline,
then reconnects with the same device ID, certificate and listener. The remaining
journey exercises actual packaged workers, transfer, address changes, restart,
lost-access isolation/repair, shared settings, pause, retention policy and removal.
The companion verifies both safe policies across an acknowledged later copy
cycle: source deletions retain destination bytes, and a destination deletion
stays absent while its source bytes remain. Ordinary Covalent still tests its
explicit opt-in deletion and restoration controls.
The ordinary Covalent branch retains its existing ordering and theme. Neither
production app's pairing, worker or file-operation implementation changes here.

Each app supplies its own debug-only theme adapter. Only the one shared journey
and its debug package bridge are staged through the Android variant generated
source API. Release source sets include neither fixture. The test uses the real
shared screen inside Compose's test activity; it does not exercise Interlink's
entry activity, top bar, large-font layout, background lifecycle or a real Pi.

Native helper preflight defaults to the original Covalent identity. The dedicated
package requires explicit `--package life.michaelwong.interlink`; both identities
still require exact installed paths, selected ABI bytes and digest manifests.
Eleven checks include the actual CLI, cross-package refusal, unsafe paths and
tampered companion helpers. Three newly needed test dependency artifacts match
Google Maven bytes over HTTPS and are pinned individually. Every prior artifact
hash and the existing verification policy remain unchanged.

Build/lint, both instrumentation classpaths and full foundation validation pass.
The first local device runner updates the exact debug APKs, then refuses before
instrumentation: Android's package substring filter returns the test package as
well as its target. Its receipt and logs remain unchanged. Explicit continuation
checks the exact target UID, installed app/test/helper bytes, original sources,
prior receipts and the same boot/framework; it performs the first instrumentation
without reinstalling either package, clearing data or replaying input.

The first actual instrumentation stops at the generic test's expectation of
deletion controls which this photo screen deliberately excludes. Its failure
and exact test source remain unchanged. The policy-corrected test builds for
both apps. Its first fresh API-37 run instead times out obtaining a folder grant;
the system confirmation remains open after the coordinate tap. Its retained
screen hierarchy shows the exact enabled ALLOW button. The next correction
waits for that DocumentsUI button and dispatches its accessibility click once,
then keeps the original exact persisted-tree checks and deadlines. It does not
inject a grant or replay accepted input. Both test classpaths build again.

The confirmation-corrected run passes both exact grants, retained peer identity,
manual/Unicode transfer, generation-1 completion and the first settings dialog.
It then fails waiting for the changed peer address to become authoritative
(184.330s JUnit / 194.612s wrapper). Boot/framework, code and previous evidence
are unchanged. This happens before the older post-address transfer-stall catch.
The address confirmation had neither a visibility/enabled wait nor keyboard
settling; the next correction observes input readiness, closes the keyboard and
uses the existing guarded dialog-button helper for one click. Its original
status deadline now reports bounded endpoint state and button readiness on
failure. Both final instrumentation classpaths compile. The separate fresh owned
API-37 run passes the complete journey in458.364s JUnit / 466.038s wrapper: both
real system grants, initial manual/Unicode copies, authenticated address/worker
port movement, restart, lost-grant repair and transfer, continuous settings,
pause/resume, both retention policies across acknowledged later cycles and
removal. Its exact app/test bytes, boot/framework, UID, source hashes and all
previous failure records verify. This single pass does not establish a fix for
the older intermittent post-address transfer stall; no production pairing,
worker or file-operation code changes here.

Completed failed emulators 5612/5614 were ended after exact ownership,
terminal-result, boot and identity-hash checks. Their AVD disks and failure
records remain retained; no app data was cleared. Evidence and frozen runners
are under
`artifacts/evidence/companion-native-journey/`. The earlier intermittent address
stall and hosted emulator permission crash remain separate and unresolved.
Source fed6107 hosted CI39644102561 now passes all86 baseline tests (72.693s)
and its ordinary-app SAF journey (422.116s), plus seven software lanes and
both CodeQL languages/policy. Permission recovery again correlates live and
DropBox reports with the known62-byte emulator graphics abort and framework
restart. Its artifact digest and all73 entries verify; gates remain closed.
These checks use synthetic files only. Destination-deletion retention is a
file-sync policy check, not evidence of Google Photos cloud backup. No physical
phone, Pi, account, original media or image changes.

The generated source wiring follows the Android
[variant source API](https://developer.android.com/reference/tools/gradle-api/9.2/com/android/build/api/variant/SourceDirectories).

Next: exercise the actual entry/scaffold and large-font layout, and correct
the top summary when a retained photo link has an empty worker-folder list
while paused/manual. The shared native journey does not cover those surfaces.
The optional after-test-home.png shows the empty synthetic emulator launcher
after the test activity exits; it is not a Pi or companion UI acceptance image.
