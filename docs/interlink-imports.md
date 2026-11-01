# Interlink drive and camera confirmation

The companion connects native drive and camera cards to the owner-bound import API.
Review reads a fresh protected context without writing. Confirmation shows the
source, file count/size, named Pi destination and usable space. Folder details
reveals the fixed relative destination; no caller path is accepted. New sources
default to import once. Remembering requires the explicit checkbox; a previously
remembered source preserves that saved choice. Pause has its own confirmation
and explains that earlier admitted work may still finish. Originals stay put.

Kotlin validates exact field sets, full identities, generations, source/action
tuples, bounds, canonical payloads and receipt digests. It never signs or grants
authority. The authenticated local native service signs with the existing phone
identity, persists the exact request before its first network send, and the Pi
independently verifies protected approval and current preview/consent. The UI
rechecks selection/pairing and the frozen source before confirmation. Changed
sources or approvals require a new review. New confirmations cannot double-submit.

Saved requests distinguish waiting, accepted, uncertain, expired, cancelled and
a later acknowledged pause. Accepted means admission only. A missing reply is
checked using the saved ID; it is never automatically sent again. Cancellation
uses its own signature and cannot pretend to undo work already admitted. Offline
history is labelled and has no active controls. A changed selection hides old
observations. Root/native approval changes leave old outcomes visibly unconfirmed;
they do not reauthorize or replay those requests. The new full approved bond can
make a fresh confirmation even if an older card has disappeared. At the bounded
history limit, older non-executable requests for that same Pi may be pruned;
no receipt is fabricated and current-bond uncertainty remains protected.

Dashboard polling runs while the screen is started, shares its operation gate
with import commands, and reads pending requests again after a command or lost
reply. Credentials/keys/addresses are never supplied by the screen. A compatible
protected camera context enables the same review flow. Legacy observations keep
the explicit unavailable state. Cameras without stable serials show why they
cannot be remembered; their only import choice is once per connection. A drive's
remember choice discloses its small identification file. Cameras are not written
to. Pending work blocks its own service journal, not another source kind.
Background reminders are described in `interlink-background-alerts.md`.
Remaining remote settings and
actual device acceptance are still required for the full headless product.

Current camera-stage software checks pass: 265 JVM tests, 179 core/349 node tests,
14 actual API36 native component tests and normal debug package provenance,
alignment/budget checks. Camera schema2 and legacy volume fixtures both verify
cryptographically in shared core. The 14 component tests use the normal debug
Application and complete native producers with synthetic source data. Camera
screens and 200% dark text are captured in `brand/`. No complete signed Pi
component bundle, physical import, account or cloud acceptance follows from
these checks. Signed read-version negotiation now preserves the volume view for
older readers; actual installed app/platform upgrade qualification remains open.

Earlier volume-stage verification: full250 JVM and345 node unit tests, zero failures/
ignored; denied-warning workspace/all-target/all-feature clippy; native API36
component instrumentation and example-data captures. Later normal-package
build, permission/reboot and 15 component checks are recorded separately in
`interlink-startup-recovery.md`. Physical Pi/phone, actual media copy, roaming,
installed update/recovery, cloud confirmation and gold-image gates remain
independent. The original visual APK has a
separate preview identity, generic Application and omitted native producers.
It proves actual Compose rendering/callbacks, not complete companion startup.

The shared core's real canonical signed fixture comes from an owned Android
integration using synthetic identities whose secrets only lived in memory.
Kotlin fixture tests check structure/canonical bytes, not cryptographic validity.
Shared core and native Pi tests independently enforce signature verification.
See appliance-import-transport.md for the wire/storage contract and docs/brand
for canonical screenshots. Private Interlink retains bounded build/test/cleanup
evidence. No real account or media is part of these fixtures.
