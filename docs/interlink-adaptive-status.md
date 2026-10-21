# Dedicated companion status and adaptive layout

An idle native worker list no longer makes an existing photo link look unset.
The photo overview uses retained links for setup, pending invitation, paused and
removing states. Folder repair, pairing upgrades and explicit lifecycle errors
take precedence. An incoming invitation still asks for a destination. These are
setup messages; none asserts an online peer or a Google Photos cloud backup.
Ordinary Covalent retains its existing running/ready summary, with explicit
`NEEDS_ATTENTION` lifecycle now treated as attention too.

Source and destination stack when the card cannot comfortably fit two columns
at the current font scale. Normal-width cards retain adjacent endpoints. The
overview stacks Refresh below its message on narrow screens or with large text.
Interlink's actual home title remains a single serif line beside its logo; its
startup/retry content can scroll in short viewports. Its body still handles
keyboard insets independently of the fixed top bar.

`artifacts/evidence/companion-adaptive-status/` records the exact sources,
builds, APKs, device and screen evidence. The debug/test APKs build and both
Android modules pass debug lint. Two companion instrumentation tests pass with
strict exact-suite validation (3.517s JUnit / 4.852s wrapper): retained idle and
paused links, repair precedence, incoming destination choice, expired/pending
and removing state, paired/unpaired setup; narrow 200% text endpoint geometry,
wide default-text geometry, and reachable/disabled Refresh. They render real
components with synthetic state inside the Compose test activity.

Separate headed checks launch the real `InterlinkActivity` on the retained owned
API37 emulator. Normal entry, 200% system text at 320dp portrait width, retained
paired source setup, keyboard-open pairing controls, and landscape pairing and
source chooser are exercised. The title bounds remain x232/y158/w530/h163 in
portrait before and after keyboard opening. Controls are reached by scrolling;
the screenshots deliberately include partial adjacent content at scroll edges.
No new pairing is submitted and no media folder is selected. Display/font and
rotation settings are restored, then the app is force-stopped. Get started
enabled its existing synthetic host; that preference remains enabled.

The in-place app/test update preserves the target UID and all five checked
private-file hashes. All eight packaged native libraries match the preceding
dedicated APK. Initial automation stops after app installation because adb
prints `Performing Push Install` before `Success`; its receipt is retained.
The continuation verifies the installed app and unchanged identity records,
installs only the test APK and performs the first instrumentation once. No data
clear, instrumentation retry or original-media operation occurs.

Starting the real host subsequently changes `network-pairing.json`: it holds
the replay floor and pending-request state, advances its replay floor on open,
and now has zero pending items. This file is distinct from retained folder-sync
peer trust. The core identity, TLS identity, engine identity and folder-sharing
files remain byte-identical; the real UI still shows one paired device.
Do not describe all five files as unchanged after host startup.

Full foundation validation passes in45.722s. Release-package verification passes
in153.950s: unsigned APK/AAB, all eight native objects, debug/release lint and
the companion license/SBOM evidence. The unsigned APK is28,856,202 bytes; it does
not establish production signing or an installed minified-app result. No full native folder journey is
replayed for these UI-only changes. Its preceding exact APK's native journey
remains recorded in `docs/interlink-native-journey.md`, with separate source
and APK boundaries. Companion instrumentation is exercised locally; hosted
API37 currently runs the ordinary Covalent suite.

Still open: the earlier intermittent post-address transfer stall, supported
permission-recovery emulator acceptance, real Pi/Solstice roaming and restart,
SD/USB/PTP/MTP imports, exact Google Photos-confirmed deletion, production
companion signing, authenticated natural updates and final gold-image cold boot.
No appliance runtime, image or physical installation changes in this milestone.
