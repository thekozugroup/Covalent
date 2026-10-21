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
