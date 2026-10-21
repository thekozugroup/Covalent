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

The later d9eb263 run 39526795075 also failed: its tap reported exit20 and
`missing-service`; the subsequent observation found no system_server PID.
Seven non-device lanes and all 87 baseline/SAF tests passed, but that is not a
passing permission journey. No crash log was retained, so the cause and tap
delivery remain unknown.

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

The harness now starts `interlink_framework_trace.py` before APK installation
or permission mutations. It requires actual log-stream readiness before
continuing. Crash/events/system/main buffers are filtered in memory into fixed
categories for framework crashes, watchdogs, known low-memory victims, process
starts/deaths and a small allowlist of exception classes. Only approved process
names, integer PIDs, device timestamps and host arrival/phase times remain.
Arbitrary messages, paths, labels, raw log lines and adb stderr are discarded.
Events and remembered PIDs are each bounded to 256 entries. Oversized lines
are dropped. The source hash is included in the final receipt.

Tap start, return code or timeout are recorded around the one actual input
command. The stream remains active through failure observation and cleanup.
Stopping it terminates only the owned adb log client, never the server, guest
or app. An unready, prematurely ended or incompletely stopped stream prevents
an otherwise successful journey from passing; it cannot hide the primary
input failure. No input is replayed and no guest is restarted. Twenty-two host
tests cover privacy, bounded storage, actual managed child-process lifecycle,
stream readiness/loss, missing-framework failures and existing input/cleanup
behavior. Resource warnings are treated as errors in the local check.
Dead or reused PIDs lose their previous attribution, preventing an unrelated
process's later exception from being labeled as a framework failure.

The `-T 1` stream can include prior buffered records. Such a record is not
automatically attributed to this test. Device time, host arrival and explicit
phases help establish ordering; even a captured crash alone does not prove its
trigger. The framework PID/boot checks and both actual permission routes remain
required. The new recorder's full API37 journey and latest-head hosted result
remain separate acceptance gates.

The final-source read-only Android36 smoke retains 396 examined lines and one
safe capture phase. Source bytes, boot ID and system_server/surfaceflinger PIDs
remain unchanged; the owned client and reader stop cleanly. This validates the
recorder, not API37 permission recovery. Evidence is retained locally in
`artifacts/interlink-qa/framework-trace-android36-final-smoke.json`.

A separate historical crash-buffer observation on that synthetic fixture shows
an actual `*** FATAL EXCEPTION IN SYSTEM PROCESS` record for the previously
observed system_server PID5529 with `java.util.concurrent.TimeoutException`.
Subsequent application failures include unqualified `DeadSystemException`.
The timeout component and trigger are unknown. No Covalent causation is inferred,
and historical records are not relabeled as a continuous passing journey.
Evidence: `framework-trace-android36-final-crash-retrospective.json` in the same
local directory. An unknown fatal SYSTEM PROCESS PID is never automatically
attributed to system_server. Imported source identity is fixed; a source change
during capture prevents acceptance.

Run the real journey only after building `:companion:assembleDebug`:

```sh
python3 -B scripts/interlink-api37-permission-test.py \
  --serial emulator-5572 --avd Interlink_QA_API37 --evidence-dir PRIVATE_DIRECTORY
```

The permission receipt proves only this synthetic emulator journey. It does
not certify phone roaming, pairing, production signing or Pi hardware.
