# Android permission recovery checks

The API37 gate exercises permission loss and both ways to restore access:
the app's runtime prompt and Settings → Permissions → Nearby devices → Allow.
Returning from Settings must restart the opted-in native host itself. The test
does not reopen the Activity or start the service to make that return pass.

`scripts/interlink-api37-permission-test.py` accepts only the named QA AVDs
at their fixed serials (including Interlink_Unlock_API37 at emulator-5592 for
fresh startup checks). It installs and reads back the exact debug APK, refuses
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
input failure. No input is replayed and no guest is restarted. Thirty host
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

## Native framework abort details

The 1e54976 hosted run39558337954 separately passes all 86 baseline tests and
the full SAF journey in 415.979 seconds. Permission recovery fails while opening
app settings. Here the Open settings tap returns0 before system_server651's
native signal6, by 2.024 host seconds. Android starts PID8025 on the same boot;
three synthetic identity digests remain unchanged. The preceding e0dfc1c abort
occurred before its failed tap. Neither ordering establishes a crash cause or
trigger. Both runs use the previous recorder without native dump details;
the new recorder still needs its own full hosted check. The verified artifact
and source-bound outcome are retained under artifacts/evidence/api37-1e54976/.


The e0dfc1c hosted recording establishes a native signal6 in the previously
observed system_server PID657 before the failed Permissions tap. It establishes
neither the initiating cause nor the trigger. The old recorder discarded the
native dump; the new follow-up decodes a bounded subset in memory. Android's
[native crash documentation](https://source.android.com/docs/core/tests/debug/native-crash)
explains the separate signal, abort-message and backtrace records.

A DEBUG dump header must match a known process whose native fatal signal was
already observed. The logging crash_dump PID remains distinct from the victim
PID. Unknown/mismatched headers and a new dump reset that logger's context.
Only fixed approved system-library names, relative code offsets, bounded build
IDs and a small fixed symbol set are emitted. Arbitrary library paths, symbols,
thread names, registers, abort messages and raw lines are discarded. Abort
messages retain a fixed prefix-family label or unclassified, their byte count
and SHA-256; that label is diagnostic metadata, not an established crash cause.
Traversal/relative/private paths are refused. There are at most 32 known fault
PIDs, 16 dump contexts and 32 unique frames per dump; omitted-frame counts remain
visible within the existing 256-event bound.

After an observed known native fault, stopping capture allows at most five
seconds for delayed native details on the same log stream. This is read-only;
input, service, acceptance and framework-stability conditions are unchanged.
A stream that dies during this drain still fails continuity. Only the owned
log client is terminated afterward. Eight new host regressions cover privacy,
correlation, context reset, limits, delayed frames and stream loss, using real
managed child processes. All thirty host tests pass with resource warnings
promoted to errors.

The source-bound Android36 read-only smoke on the retained boot reads two idle
log lines and one capture marker, stops its reader/client cleanly, and preserves
boot ID plus both framework PIDs/start ticks. It validates stream operation;
it has no native dump and does not prove the API37 failure cause or permission
recovery. Evidence: `artifacts/interlink-qa/native-framework-smoke-1792566867768376000.json`.
The earlier native crash and all failed journey artifacts remain retained.

The d57deda hosted run39562643351 is now terminal. All seven non-device jobs,
86 baseline tests and the complete SAF journey pass (437.290 seconds). The
permission journey still fails. Its continuous recorder observes the known
system_server1751 native signal6, 1.472 host seconds after Open settings returns0
and before the failed Permissions tap. Android starts another system_server;
the harness does not restart it. The three synthetic identity digests remain
unchanged. Permission restoration fails with missing framework services.

The five-second native-detail drain completes on the original live stream, but
the retained output has no recognized native dump header, frame, signal-detail
or abort-summary event. This does not distinguish unavailable logs from an
unrecognized dump and does not establish the cause or trigger. Artifact
12329751390 has verified ZIP SHA-256
`7d767dcce5f8f3b790a41f190721e843f9f8973152f7146bacb09300ef713927`.
Original reports and source-bound outcome are retained under
`artifacts/evidence/api37-d57deda/`. Permission recovery and the release gate
remain failed; no deadline extension, input replay, app fix or readiness claim
is inferred from the passing baseline/SAF checks.
