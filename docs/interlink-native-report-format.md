# Android native-report header compatibility

The failure reader now recognizes Android 37's optional parent-process field.
It previously rejected a real report even when its timestamp, victim and signal
matched the observed fault. Both the live logger and DropBox reader shared
this limitation through duplicated header expressions.

The accepted forms are:

```text
pid: PID, tid: TID, name: THREAD  >>> PROCESS <<<
pid: PID, ppid: PARENT, tid: TID, name: THREAD  >>> PROCESS <<<
```

Both paths now use one compiled grammar. The optional parent must contain one
to ten decimal digits. Its value is ignored and never retained. Victim PID,
crashed TID, process and signal correlation, report timestamp/window and complete
read requirements remain unchanged. Malformed parents, extra fields, changed
process identities and leading/unsupported syntax remain refused.

## Actual Android reproduction and follow-up

A new account-free `Interlink_Native_Report_API 37` at emulator-5600 uses the
installed API 37 Google Play ARM64 image. It changes no earlier AVD or physical
device. A separate 7080-byte native fixture sets its own fixed test process
name, reports its PID and calls `abort()` once. It contains no sync, account,
network, app-data or critical-service code. Android records native signal 6 in
that actual PID 5974/TID 5974; its ADB child exits 134.

The 8a17a76 reader observes six complete reads of the resulting 18,081-byte
`SYSTEM_TOMBSTONE` report. Each has one in-window timestamp and one rejected
header. The original receipt remains failed. A separate structural inspection
retains only fixed approved field names and confirms `pid, ppid, tid, name`;
raw report text and the parent identifier are not stored.

After the grammar fix, a new read-only follow-up reads this same existing
report on the same boot. It authenticates the original source hashes, terminal
fixture exit and actual fatal-signal record. One complete report now matches
the original PID/TID/process/signal and observed timestamp window, retaining
four approved metadata events including two libc frames. The private fixture
frame is omitted. System-server/surfaceflinger PIDs and start ticks remain
identical to the original pre-fault snapshot. The native fault is not replayed
and no process, guest, APK or service is restarted.

After accepted readback, cleanup verifies the exact shell-owned private
directory, sole executable, mode, link count and binary hash, then removes only
that executable and directory. The native report remains available in this
new disposable AVD. Original failed receipts and all host artifacts remain.

Original failed probe:
`artifacts/evidence/native-report-counts/native-android-1792592652029512000/receipt.json`.
Separate passing follow-up:
`artifacts/evidence/native-report-parent-header/android-followup-1792593138131073000/receipt.json`.
Source/evidence binding:
`artifacts/evidence/native-report-parent-header/manifest.json`.

## Verification limits

All 73 focused tests pass on both macOS and Linux: 35 reader/parser tests,
32 permission-harness tests and six forward-cleanup tests. Three new cases
cover identical output for old/new headers, malformed/extra parent fields and
the shared live/report grammar. Resource warnings are errors. No APK rebuild
is needed because this change affects QA diagnostics only.

This proves real Android native-report formatting and redacted parsing for
one owned test process. It does not reproduce the hosted system_server crash,
establish its cause or repair permission recovery. No production collector
allowlist is expanded; that collector still accepts only its approved framework
faults on named test emulators. Existing failed hosted runs stay failed. The
new reader's actual hosted framework-report capture and full permission journey
remain required before any merge/release acceptance.
