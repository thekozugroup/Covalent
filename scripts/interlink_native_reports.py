"""Bounded, read-only DropBox fallback. Raw reports never leave memory."""
from datetime import datetime
import hashlib
import os
from pathlib import Path
import re
import selectors
import subprocess
import time

from interlink_framework_trace import NativeDumpDecoder, SOURCE_SHA256 as DECODER_SHA256

SOURCE_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
OWNED_SERIALS = frozenset(("emulator-5570", "emulator-5572", "emulator-5592"))
TAGS = ("system_server_native_crash", "SYSTEM_TOMBSTONE")
MAX_BYTES = 1024 * 1024
MAX_LINE = 8192
TIMEOUT = 8
MAX_EVENTS = 96
STAMP = re.compile(r"Timestamp: (\d{4}-\d\d-\d\d \d\d:\d\d:\d\d(?:\.\d{1,9})?[+-]\d{4})")
HEADER = re.compile(r"pid: ([0-9]{1,10}), tid: ([0-9]{1,10}), name: .{0,256}  >>> ([^<>]{1,128}) <<<")


class ReportParser(NativeDumpDecoder):
    def __init__(self, faults, start, end):
        super().__init__()
        self.faults = faults
        self.native_faults = {pid: value["process"] for pid, value in faults.items()}
        self.start, self.end = start, end
        self.timestamp = None
        self.pending = None
        self.admitted = False
        self.events = []
        self.dropped = 0
        self.lines = 0
        self.reports = 0
        self.diagnostic_counts = dict.fromkeys((
            "reportBoundaries", "overlongLines", "timestampLines", "timestampPrefixRejected",
            "timestampSyntaxRejected", "timestampParseRejected", "timestampBeforeWindow",
            "timestampAfterWindow", "timestampInWindow", "headerLines", "headerSyntaxRejected",
            "headerWithoutWindowTimestamp", "headerPidRejected", "headerTidRejected",
            "headerProcessRejected", "headersCorrelated", "signalLinesWithHeader",
            "signalSyntaxRejectedWithHeader", "signalRejectedWithHeader"), 0)

    def count(self, name):
        # Fixed categories and bounded integers only; rejected text stays private.
        self.diagnostic_counts[name] = min(65535, self.diagnostic_counts[name] + 1)

    def reset(self):
        self.timestamp = self.pending = None
        self.admitted = False
        self.native_dumps.clear()

    def consume(self, line):
        self.lines += 1
        if len(line) > MAX_LINE:
            self.count("overlongLines")
            self.reset()
            return
        if line.startswith("*** ***") or line.startswith("========================================"):
            self.count("reportBoundaries")
            self.reset()
            return
        if not line.startswith("Timestamp:") and line.lstrip().startswith("Timestamp:"):
            self.count("timestampPrefixRejected")
        if line.startswith("Timestamp:"):
            self.count("timestampLines")
            self.reset()
            match = STAMP.fullmatch(line)
            if match:
                try:
                    epoch = int(datetime.fromisoformat(match[1]).timestamp())
                    if self.start <= epoch <= self.end:
                        self.timestamp = epoch
                        self.count("timestampInWindow")
                    else:
                        self.count("timestampBeforeWindow" if epoch < self.start else "timestampAfterWindow")
                except (ValueError, OverflowError):
                    self.count("timestampParseRejected")
            else:
                self.count("timestampSyntaxRejected")
            return
        if line.lstrip().startswith("pid:"):
            self.count("headerLines")
            self.admitted = False
            self.pending = None
            self.native_dumps.clear()
            match = HEADER.fullmatch(line)
            if not match:
                self.count("headerSyntaxRejected")
            if self.timestamp is None:
                self.count("headerWithoutWindowTimestamp")
            if self.timestamp is None or not match:
                return
            fault = self.faults.get(int(match[1]))
            if not fault:
                self.count("headerPidRejected")
                return
            if fault["tid"] != int(match[2]):
                self.count("headerTidRejected")
                return
            if fault["process"] != match[3]:
                self.count("headerProcessRejected")
                return
            self.pending = self._native_dump(0, line, {"origin": "dropbox", "timestampEpochSeconds": self.timestamp})
            if self.pending:
                self.count("headersCorrelated")
            return
        if not self.pending:
            return
        event = self._native_dump(0, line, {"origin": "dropbox", "timestampEpochSeconds": self.timestamp})
        if line.lstrip().startswith("signal "):
            self.count("signalLinesWithHeader")
            if event is None:
                self.count("signalSyntaxRejectedWithHeader")
        if event is None:
            return
        if event["kind"] == "native-dump-signal":
            if event["signal"] != self.faults[event["victimPid"]]["signal"]:
                self.count("signalRejectedWithHeader")
                self.reset()
                return
            if not self.admitted:
                self.reports += 1
                self.append(self.pending)
                self.admitted = True
        if self.admitted:
            self.append(event)

    def append(self, event):
        if len(self.events) < MAX_EVENTS:
            self.events.append(event)
        else:
            self.dropped += 1


def bounded_read(command, consume, *, max_bytes=MAX_BYTES, timeout=TIMEOUT):
    """One owned client, fixed byte/deadline bounds, incremental redaction."""
    state = {"bytesRead": 0, "limitReached": False, "timedOut": False,
             "exitCode": None, "clientStopped": False, "failureClass": None}
    proc = None
    pending = b""
    discard = False
    deadline = time.monotonic() + timeout
    try:
        proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        os.set_blocking(proc.stdout.fileno(), False)
        with selectors.DefaultSelector() as selector:
            selector.register(proc.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    state["timedOut"] = True
                    break
                if not selector.select(min(remaining, 0.1)):
                    continue
                data = os.read(proc.stdout.fileno(), min(65536, max_bytes - state["bytesRead"] + 1))
                if not data:
                    if pending and not discard:
                        consume(pending.decode("utf-8", errors="replace"))
                    break
                state["bytesRead"] += len(data)
                if state["bytesRead"] > max_bytes:
                    state["limitReached"] = True
                    break
                # A long line is discarded in full; no suffix may become a header.
                parts = (pending + data).split(b"\n")
                pending = parts.pop()
                for part in parts:
                    if not discard:
                        consume(part.rstrip(b"\r").decode("utf-8", errors="replace") if len(part) <= MAX_LINE else "X" * (MAX_LINE + 1))
                    discard = False
                if len(pending) > MAX_LINE:
                    consume("X" * (MAX_LINE + 1))
                    pending = b""
                    discard = True
        if not state["timedOut"] and not state["limitReached"]:
            proc.wait(timeout=max(0.01, deadline - time.monotonic()))
    except subprocess.TimeoutExpired:
        state["timedOut"] = True
    except (OSError, ValueError) as error:
        state["failureClass"] = type(error).__name__
    finally:
        if proc is not None:
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=1)
            state["exitCode"] = proc.returncode
            state["clientStopped"] = proc.poll() is not None
            proc.stdout.close()
    return state


def collect(adb, serial, boot_id, start, trace):
    """Diagnostic evidence cannot change a failing permission result into a pass."""
    if serial not in OWNED_SERIALS or not re.fullmatch(r"[a-f0-9-]{36}", boot_id) or type(start) is not int or start <= 0:
        raise ValueError("Owned emulator and capture identity required")
    result = {"sourceSha256": SOURCE_SHA256, "decoderSha256": DECODER_SHA256,
              "rawReportsRetained": False, "result": "unavailable", "attempts": []}
    faults = {}
    for event in trace.get("events", []):
        if event.get("kind") == "native-fatal-signal" and event.get("process") in {"system_server", "surfaceflinger"}:
            pid, tid, signal = (event.get(key) for key in ("sourcePid", "sourceTid", "signal"))
            if all(type(value) is int and value > 0 for value in (pid, tid, signal)):
                faults[pid] = {"tid": tid, "signal": signal, "process": event["process"]}
    if not faults:
        return {**result, "result": "not-needed"}
    if len(faults) > 32:
        return {**result, "reason": "fault-limit"}

    def scalar(*args):
        lines = []
        state = bounded_read([adb, "-s", serial, "shell", *args], lines.append, max_bytes=128, timeout=3)
        if state["exitCode"] != 0 or state["limitReached"] or state["timedOut"] or state["failureClass"] or len(lines) != 1:
            raise RuntimeError("Identity observation unavailable")
        return lines[0]

    try:
        if scalar("cat", "/proc/sys/kernel/random/boot_id") != boot_id:
            return {**result, "reason": "boot-changed"}
        epoch = scalar("date", "+%s")
        if not re.fullmatch(r"[1-9][0-9]{0,11}", epoch) or int(epoch) < start:
            return {**result, "reason": "clock-unavailable"}
        result["windowEpochSeconds"] = [start, int(epoch)]
        for tag in TAGS:
            parser = ReportParser(faults, start, int(epoch))
            format_seen = {"serviceHeader": False, "searchTag": False, "entryOrEmpty": False, "reportReadError": False}

            def consume(line):
                if re.fullmatch(r"Drop box contents: [0-9]+ entries", line):
                    format_seen["serviceHeader"] = True
                elif line == "Searching for: " + tag:
                    format_seen["searchTag"] = True
                elif line == "(No entries found.)" or line == "========================================":
                    format_seen["entryOrEmpty"] = True
                elif line.startswith("*** ") and not line.startswith("*** ***"):
                    format_seen["reportReadError"] = True
                parser.consume(line)

            state = bounded_read([adb, "-s", serial, "shell", "dumpsys", "dropbox", "--print", tag], consume)
            recognized = all(format_seen[k] for k in ("serviceHeader", "searchTag", "entryOrEmpty"))
            complete = state["exitCode"] == 0 and state["clientStopped"] and recognized and not format_seen["reportReadError"] and not any(state[k] for k in ("timedOut", "limitReached", "failureClass"))
            result["attempts"].append({"tag": tag, **state, "complete": complete, "linesExamined": parser.lines,
                "matchingReports": parser.reports, "eventsDropped": parser.dropped,
                "diagnosticCounts": dict(parser.diagnostic_counts),
                "nativeFramesOmitted": parser.native_frames_omitted, "formatRecognized": recognized,
                "reportReadError": format_seen["reportReadError"], "events": parser.events})
        if scalar("cat", "/proc/sys/kernel/random/boot_id") != boot_id:
            return {**result, "reason": "boot-changed"}
        if hashlib.sha256(Path(__file__).read_bytes()).hexdigest() != SOURCE_SHA256:
            return {**result, "reason": "source-changed"}
        if hashlib.sha256(Path(__file__).with_name("interlink_framework_trace.py").read_bytes()).hexdigest() != DECODER_SHA256:
            return {**result, "reason": "decoder-changed"}
        complete = [attempt for attempt in result["attempts"] if attempt["complete"]]
        result["result"] = "matched" if any(a["matchingReports"] for a in complete) else "no-match" if len(complete) == len(TAGS) else "unavailable"
        return result
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        return {**result, "failureClass": type(error).__name__}
