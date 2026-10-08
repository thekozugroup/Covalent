# Android appliance integration — 0.2.4

The Android release retains the complete JNI registration surface under R8. Previously, removing the apparently unused `nativeState` method caused `JNI_OnLoad` to reject the entire native class, leaving folder sync waiting for startup.

This build also prefers a VPN Tailnet IPv4 address, restarts the embedded node when the usable route changes, and maintains Android multicast permission independently of backup mode. Discovery waits three seconds and excludes the local device and unusable endpoints. Pairing uses persistent device identities; Tailscale must be connected on both devices for their private addresses to work.

The app now uses the shared overlapping-circle brand mark, adaptive and themed icons, a fixed slate palette, clearer folder selection, collapsed advanced options, and an explicit startup retry action. Deletion propagation remains off by default.

## Verification on 2026-10-08

- Strict dependency verification and release assembly passed; 198 Android unit tests passed.
- The minified, non-debuggable release started on a physical Android phone, paired with a Raspberry Pi appliance through Tailscale addresses, selected Pictures using Android's folder permission screen, and transferred files.
- APK SHA-256: `d6e3cc371bd09927608a6d3b022be69da2632f22f5720198e83c9f5b88900a0d`.
- The receiving appliance offered an editable destination folder before acceptance. That appliance implementation is maintained separately in Interlink.
- Full Pictures completion, whole-folder hash comparison, discovery from the phone, and Wi-Fi-to-cellular handoff remain acceptance checks. This is a test build, not a completed production certification.

No account credentials, signing keys, pairing secrets, or personal photos are included in this change.
