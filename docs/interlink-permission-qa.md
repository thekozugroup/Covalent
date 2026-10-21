# Android permission recovery checks

The API37 gate exercises permission loss and both ways to restore access:
the app's runtime prompt and Settings → Permissions → Nearby devices → Allow.
Returning from Settings must restart the opted-in native host itself. The test
does not reopen the Activity or start the service to make that return pass.

`scripts/interlink-api37-permission-test.py` accepts only the two named QA AVDs
at their fixed serials. It installs and reads back the exact debug APK, refuses
an existing non-debug app, requires the unpaired native-ready screen and a
foreground service, and compares device/TLS/key-envelope hashes. It never clears
app data. A permission left revoked by a failure is restored during cleanup.

The boot ID and actual system_server/surfaceflinger PIDs must remain unchanged
through the complete test. Framework restarts cannot turn a failed journey
into a pass. No automatic emulator restart or input replay is performed.

## Failure evidence

Hosted run 39520547422 at source 126cec9 passed the 86 baseline tests and SAF
journey but failed at `adb shell input tap` with exit 20 in this additional
check. Its original receipt discarded stdout/stderr, so the cause and whether
the tap was delivered are unknown. The full release gate failed.

Failures now retain a safe command label, exit code, known exception classes,
fixed diagnostic categories and stdout/stderr hashes. Raw command arguments,
output, private files, account labels and arbitrary UI text are excluded. A
fresh allowlisted UI observation, framework IDs and protected identity hashes
are captured before cleanup where available. An unavailable hierarchy is
explicit; a preceding hierarchy is never substituted.

A nonzero input command still fails the test even if its subsequent screen is
observed. The evidence distinguishes those outcomes for diagnosis without
weakening acceptance. `scripts/test-interlink-permission-harness.py` exercises
both outcomes, absent hierarchy, redaction and foreign-serial refusal without
contacting a device. Android foundation CI runs these checks before building.

Run the real journey only after building `:companion:assembleDebug`:

```sh
python3 -B scripts/interlink-api37-permission-test.py \
  --serial emulator-5572 --avd Interlink_QA_API37 --evidence-dir PRIVATE_DIRECTORY
```

The permission receipt proves only this synthetic emulator journey. It does
not certify phone roaming, pairing, production signing or Pi hardware.
