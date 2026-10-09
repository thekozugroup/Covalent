# Android appliance integration — 0.2.4

The Android release retains the complete JNI registration surface under R8. Previously, removing the apparently unused `nativeState` method caused `JNI_OnLoad` to reject the entire native class, leaving folder sync waiting for startup.

This build also prefers a VPN Tailnet IPv4 address, restarts the embedded node when the usable route changes, and maintains Android multicast permission independently of backup mode. Discovery waits three seconds and excludes the local device and unusable endpoints. Pairing uses persistent device identities; Tailscale must be connected on both devices for their private addresses to work.

The app now uses the shared overlapping-circle brand mark, adaptive and themed icons, a fixed slate palette, clearer folder selection, collapsed advanced options, and an explicit startup retry action. Deletion propagation remains off by default.

## Verification on 2026-10-08

- Strict dependency verification and release assembly passed; 198 Android unit tests passed.
- The minified, non-debuggable release started on a physical Android phone, paired with a Raspberry Pi appliance through Tailscale addresses, selected Pictures using Android's folder permission screen, and transferred files.
- Initial physical test APK SHA-256: `d6e3cc371bd09927608a6d3b022be69da2632f22f5720198e83c9f5b88900a0d`.
- The receiving appliance offered an editable destination folder before acceptance. That appliance implementation is maintained separately in Interlink.
- Full Pictures completion, whole-folder hash comparison, discovery from the phone, and Wi-Fi-to-cellular handoff remain acceptance checks. This is a test build, not a completed production certification.

No account credentials, signing keys, pairing secrets, or personal photos are included in this change.

## CI follow-up

Paired-device counts use Android plurals, and adaptive icons use the unqualified resource directory supported by the minimum Android version. Rust formatting is normalized. The pinned Alpine Python package and its version assertion move together to 3.12.15-r0 because 3.12.14-r0 is no longer in the repository. Android debug lint and all 198 unit tests pass; the updated package resolves in a disposable pinned Alpine container. Full GitHub CI remains the merge gate.

## Startup and test repair — 2026-10-09

Android can create the application before its activity is visible, including while the screen is locked after an earlier unlock. Foreground-service denial now defers the provider without erasing its opt-in, credentials, or prior running state. The activity retries on resume. Unrelated storage and permission failures still propagate. Device tests exercise denial followed by a successful retry and propagation of an unrelated failure.

The real SAF folder journey now selects the folder chooser by its semantic tag. It no longer references the removed button string or ambiguously matches the identical heading. Strict release assembly, debug/release lint, all 198 unit tests, and instrumentation compilation pass locally. Physical-device and hosted checks follow publication.

## Mobile links view — 2026-10-09

Saved links lead the screen. New link opens pairing, source-folder selection, destination selection and optional controls. Each link card shows source and destination devices, the local folder name, status and settings. A destination's private path remains on that device; the source labels it as chosen on the destination rather than guessing a path. Settings stay available in each destination card, with an explicit notice when they affect multiple destinations of the same source folder.

The main screen uses the shared logo and a serif Covalent wordmark. Back navigation remains on secondary screens and returns to the links screen in folder-only mode. Accepting or repairing a share clears the transient folder choice so the next received share requires a fresh selection. The SAF journey opens advanced options before checking their controls.

The links view keeps observing startup after a stale saved local API address fails during an app update. Previously the first failure stopped status refresh permanently, so a running service still appeared to need startup. Continued observation does not restart the node, reset pairing, or change sync settings.
