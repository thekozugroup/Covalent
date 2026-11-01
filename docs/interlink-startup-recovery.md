# Startup after unlock

The shared Android unlock gate now closes the gap between checking the user's
unlock state and registering its listener. Previously an unlock in that gap
could be missed permanently, leaving startup waiting until the app was opened
again. This is separate from the hosted API37 framework abort: its cause
remains unestablished.

The gate checks again after registration completes. It removes the observer
before running the action, and delivers the action once even if callbacks race
or repeat. A callback received while credential storage remains locked cannot
start the action. An action failure cannot leave a callback that repeats it.
Credentials remain in credential-encrypted storage. Existing opt-in checks,
background-start restrictions, pairing and service ownership still apply.

`UserUnlockGateTest` exercises missed broadcasts, callbacks during registration,
concurrent/repeated callbacks, still-locked notifications and failure cleanup.
Five of seven checks fail against a model of the former callback behavior;
all seven pass against the repaired gate. These are deterministic callback tests,
not an old-APK reproduction or proof of a physical phone's first unlock after reboot.

Android's [Direct Boot guidance](https://developer.android.com/privacy-and-security/direct-boot)
describes the unlock broadcast and the credential-storage boundary.

## Real emulator reboot

Build both apps with `./scripts/check-android.sh`, then run the permission journey
on the fresh `Interlink_Unlock_API37` fixture at `emulator-5592`. Preserve the
previously accepted fixtures. After that journey passes:

```sh
python3 -B scripts/interlink-api37-reboot-test.py \
  --permission-receipt PRIVATE_PERMISSION_DIRECTORY/permission-recovery.json \
  --evidence-dir NEW_PRIVATE_REBOOT_DIRECTORY
```

The reboot check admits only that named API37 emulator and an exact passing
permission receipt from the same boot/APK/identity. It attempts one real reboot,
then observes the existing guest. It never launches the app, sends a synthetic
boot broadcast, starts a service or clears data. The opted-in foreground host
must return automatically, answer its native status API as ready and retain
stable app/framework PIDs for twenty seconds while its Activity stays unresumed.
The check reads only the loopback
endpoint from the synthetic fixture's preferences, bounds the status response,
discards other values and removes only its own ADB forwards. Three private
identity-file hashes, installed APK and app UID must
remain unchanged. Another reboot, framework restart, source change or loss
after readiness fails acceptance. An existing evidence directory is refused.
Raw logs and credential bytes are not retained.

This proves ordinary reboot startup on one synthetic emulator. It does not
prove a real locked-user unlock, phone roaming, authenticated transfers, Pi
hardware, source cleanup or production signing. A deadline expiry retains the
same guest for inspection and never causes another reboot attempt.

## Verified software milestone

The full Android build passes in 6m55s using the pinned Go1.26.10 compiler:
212 debug and two release unit tests, both apps' debug/release compilation, lint,
native provenance and package checks. Release artifacts remain unsigned candidates.
All thirty permission-harness tests and six forward-cleanup tests pass. Five
storage checks also pass on the actual API37 emulator; their sealed-storage
cases use a context double and do not claim a real locked-user boot.

On the fresh arm64 fixture, both actual permission routes pass with stable
framework processes and unchanged identity digests. The final reboot journey
then passes independently: one real reboot, automatic foreground host/native API
readiness, 21.126 seconds of stable observation, unresumed app Activity, unchanged
APK/UID/three identity hashes, and verified cleanup. Debug APK SHA-256:
`f7f6b62362c8abfaf5d4b132b9fa17c32a48a270ace11d314d94cdc8bfb6ab9b`.

The preceding reboot's native checks passed for 20.373 seconds, but its overall
receipt remains failed: unconditional removal treated an already absent ADB
forward as an error. A separate same-boot follow-up verified zero forwards,
native readiness, exact owned removal/readback and already-absent handling.
Cleanup now checks exact ownership and actual absence, refuses changed/foreign
mappings, and publishes a pass only after cleanup completes. The final complete
journey above uses that corrected harness and a new permission receipt; neither
original failure nor follow-up is relabeled. The initial old-Go build failure
and instrumentation admission error before installation also remain retained.

Source snapshots, original/final receipts and build/test hashes are bound by
`artifacts/evidence/android-unlock-startup.json`. Hosted x86 API37 permission
recovery remains unaccepted because of its separately recorded framework abort.
Physical devices, real first unlock, roaming and production signing remain open.


## Complete companion qualification — 2026-11-01

The normal companion now has fresh full-package evidence beyond the separate
visual preview. Its compiled application inputs are exactly c3f1ba0cf982b7bb7cf148f449206d98fc41b98e;
this iteration changes the release checker, not those APK inputs. The build ran
both real JNI and maintained worker/guardian producers, without manifest,
Application or package overrides. Debug, minified unsigned release, release AAB,
SBOM, both lint variants, native provenance/alignment/budgets and 250 JVM tests
pass in 530.456s. Lint has zero errors and 11 debug/12 release warnings. The
release APK is 29,511,238 bytes. APK/AAB native byte identity passes for both ABIs.

The release checker now requires the actual Interlink Application and launcher,
and excludes the debug preview Activity. A missing/generic/other Application,
ambiguous name/element, child attribute masquerading as the Application, missing/
other/multiple launchers and a leaked preview are refused. Eleven invalid
variants fail against the old checker; all 29 final policy tests pass. Real
SDK 36/37 signing smokes pass with disposable test keys, preserving existing
output and rejecting an Android Debug signer. These are test signatures;
production companion signing remains unconfigured.

The exact normal debug APK is installed and read back independently:
`231001c0df5f7a027252257f6e1553a1960f916390426dea3b1557df57c01a00`.
On a fresh API 37 ARM64/4-KiB-page emulator, its real launcher reaches Get started
without starting the foreground host before opt-in. The canonical
[screen capture](brand/interlink-first-launch.png) contains no account or pairing.
The actual Settings-return and runtime permission routes both recover native
readiness with unchanged device/TLS/key-envelope hashes and framework processes
in 64.380s. One real reboot then passes in 57.754s: no Activity launch, no direct
service-start command, native status ready, stable app/framework observation
for 20.912s, unchanged APK/UID/identity hashes, and independently removed forwards.
Fifteen native dashboard/import/notification/layout checks pass in 29.429s against
this normal package/Application. Their card data is synthetic; they do not
establish an actual paired import.

Private platform audit: `artifacts/validation/2026-11-01-native-companion-startup`.
The source files, package hashes, commands, original failures and cleanup are
bound there. Normal module outputs, owned apps/AVD/runtime and scratch are
removed after evidence retention. One verified pinned Go1.26.10 SDK remains
reusable outside iCloud; the default Go installation is unchanged.

This is local unpaired emulator acceptance. Hosted c3f1ba0 run 41289561149 still
fails: 85/86 general Covalent baseline tests pass; the remaining Back-navigation
check loses window focus before the companion permission journey can run. Both
container image scans also fail. The prior snapshot-verification failure and
this newer focus failure remain separate. Private platform b695b75 run 41290102402
passes both Android owner/staging and Linux recovery/media jobs. No hosted
failure is waived by the local companion result. Actual Solstice/Pi pairing,
roaming, background drive alerts, physical imports/updates, exact Google Photos
confirmation and fresh-image acceptance remain open. No gold image, physical
installation or production-signed companion release is claimed.
