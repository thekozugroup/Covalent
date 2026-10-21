"""Bounded, read-only window focus evidence; never replaces instrumentation's result."""
from collections import deque
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import threading
import time

from interlink_native_reports import bounded_read, SOURCE_SHA256 as READER_SHA256, DECODER_SHA256 as READER_DEPENDENCY_SHA256

SOURCE_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
OWNED_AVDS = {"emulator-5570": "Covalent_API_37", "emulator-5622": "Interlink_Back_Focus_API37"}
SAMPLE_SECONDS = .5
MAX_SAMPLES, MAX_FAILURES, FAILURE_HISTORY = 256, 16, 32
MAX_LOG_CHUNK, MAX_LINE = 256 * 1024, 8192
FLAGS = ("mShowingDream", "mDreamingLockscreen", "mShowingLockscreen", "mKeyguardShowing",
         "mAwake", "mScreenOnEarly", "mScreenOnFully", "mDisplayFrozen",
         "mKeyguardOccluded", "mPendingKeyguardOccluded")


def owner(value):
    if value == "null":
        return "none"
    # WindowState prints its title, which need not be a package/component.
    # Admit only exact public platform titles; never retain arbitrary titles.
    title = re.fullmatch(r"Window\{[a-fA-F0-9]+ u[0-9]+ (NotificationShade|StatusBar)(?: EXITING)?\}", value)
    if title:
        return {"NotificationShade": "notification-shade", "StatusBar": "status-bar"}[title[1]]
    match = re.search(r"(?:^|\s)([A-Za-z0-9_.]+)/([A-Za-z0-9_.$]+)(?:\s|\})", value)
    if not match:
        return "unrecognized"
    package, activity = match.groups()
    if package in {"life.michaelwong.covalent", "life.michaelwong.interlink"}:
        if activity == "androidx.activity.ComponentActivity":
            return "compose-activity"
        return "app-main" if activity in {".MainActivity", "life.michaelwong.covalent.MainActivity"} else "app-other"
    if package in {"life.michaelwong.covalent.test", "life.michaelwong.interlink.test"}:
        return "test-invoker" if activity.startswith("androidx.test.core.app.InstrumentationActivityInvoker$") else "test-other"
    return {
        "com.android.systemui": "system-ui", "com.android.settings": "settings",
        "com.android.permissioncontroller": "permission-controller",
        "com.google.android.permissioncontroller": "permission-controller",
        "com.android.documentsui": "documents", "com.google.android.documentsui": "documents",
        "com.android.launcher3": "launcher", "com.google.android.apps.nexuslauncher": "launcher",
    }.get(package, "other-redacted")


class WindowFields:
    def __init__(self):
        self.values = {}
        self.keyguard_indent = None

    def add(self, key, value):
        previous = self.values.get(key)
        self.values[key] = value if previous is None or previous == value else "ambiguous"

    def consume(self, line):
        if len(line) > MAX_LINE:
            self.keyguard_indent = None
            return
        indent = len(line) - len(line.lstrip(" "))
        if line.strip() == "KeyguardServiceDelegate":
            self.keyguard_indent = indent
            return
        if line.strip() and self.keyguard_indent is not None:
            if indent <= self.keyguard_indent:
                self.keyguard_indent = None
            else:
                guard = re.fullmatch(r"\s*(showing|inputRestricted|occluded)=(true|false)", line)
                if guard and indent == self.keyguard_indent + 2:
                    self.add({"showing": "keyguardShowing", "inputRestricted": "keyguardInputRestricted",
                              "occluded": "keyguardOccluded"}[guard[1]], guard[2] == "true")
        match = re.fullmatch(r"\s*(mCurrentFocus|mFocusedApp)=(.{1,1024})", line)
        if match:
            self.add(match[1], owner(match[2]))
        match = re.fullmatch(r"\s*mTopFocusedDisplayId=([0-9]{1,4})\s*", line)
        if match:
            self.add("mTopFocusedDisplayId", int(match[1]))
        for flag in FLAGS:
            for match in re.finditer(r"(?:^|\s)" + flag + r"=(true|false)(?=\s|$)", line):
                self.add(flag, match[1] == "true")

    def result(self):
        return {"currentFocus": self.values.get("mCurrentFocus", "unreported"),
                "focusedApp": self.values.get("mFocusedApp", "unreported"),
                **{key: value for key, value in self.values.items() if key not in {"mCurrentFocus", "mFocusedApp"}}}


def read_window(adb, serial):
    fields = WindowFields()
    read = bounded_read([adb, "-s", serial, "shell", "dumpsys", "window"],
                        fields.consume, max_bytes=256 * 1024, timeout=2)
    complete = read["exitCode"] == 0 and read["clientStopped"] and not any(
        read[key] for key in ("timedOut", "limitReached", "failureClass"))
    return {"read": read, "window": fields.result() if complete else {
        "currentFocus": "unavailable", "focusedApp": "unavailable"}}


class FocusRecorder:
    """One sampler and one owned bounded adb client at a time; no raw dumps retained."""
    def __init__(self, adb, serial, log):
        if serial not in OWNED_AVDS:
            raise ValueError("Owned instrumentation emulator required")
        self.adb, self.serial, self.log = adb, serial, Path(log)
        self.stop_event = threading.Event()
        self.thread = None
        self.started = time.monotonic()
        self.samples = deque(maxlen=MAX_SAMPLES)
        self.failures = []
        self.samples_dropped = self.failures_dropped = 0
        self.position, self.pending, self.discard = 0, b"", False
        self.ordinal, self.focus_exception = None, False
        self.log_identity = None
        self.error = None

    def elapsed(self):
        return round((time.monotonic() - self.started) * 1000)

    def consume_status(self, line):
        match = re.fullmatch(r"INSTRUMENTATION_STATUS: current=([1-9][0-9]{0,4})", line)
        if match:
            self.ordinal = int(match[1])
        if line.startswith("INSTRUMENTATION_STATUS: stack=androidx.test.espresso.base.RootViewPicker$RootViewWithoutFocusException:"):
            self.focus_exception = True
        match = re.fullmatch(r"INSTRUMENTATION_STATUS_CODE: (-?[0-9]{1,2})", line)
        if not match:
            return
        code = int(match[1])
        if code in {-1, -2}:
            event = {"hostElapsedMs": self.elapsed(), "testOrdinal": self.ordinal, "statusCode": code,
                     "rootWithoutFocus": self.focus_exception, "precedingSamples": list(self.samples)[-FAILURE_HISTORY:]}
            if len(self.failures) < MAX_FAILURES:
                self.failures.append(event)
            else:
                self.failures_dropped += 1
        self.focus_exception = False
        if code != 1:
            self.ordinal = None

    def poll_log(self):
        # The caller owns the open, append-only phase log. Never follow a link,
        # rewind a truncated log, retain stack text, or read unbounded output.
        with os.fdopen(os.open(self.log, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as stream:
            info = os.fstat(stream.fileno())
            if self.log.is_symlink() or not stat.S_ISREG(info.st_mode):
                raise ValueError("Unrecognized phase log")
            identity = (info.st_dev, info.st_ino)
            if self.log_identity is None:
                self.log_identity = identity
            if identity != self.log_identity or info.st_size < self.position:
                raise ValueError("Phase log changed")
            stream.seek(self.position)
            raw = stream.read(MAX_LOG_CHUNK + 1)
            if len(raw) > MAX_LOG_CHUNK:
                raise ValueError("Phase log read limit")
            self.position += len(raw)
        parts = (self.pending + raw).split(b"\n")
        self.pending = parts.pop()
        for part in parts:
            if not self.discard and len(part) <= MAX_LINE:
                self.consume_status(part.rstrip(b"\r").decode("utf-8", errors="replace"))
            self.discard = False
        if len(self.pending) > MAX_LINE:
            self.pending, self.discard = b"", True

    def sample(self):
        before = self.elapsed()
        observation = read_window(self.adb, self.serial)
        sample = {"hostElapsedMs": before, "completedElapsedMs": self.elapsed(), **observation}
        if len(self.samples) == MAX_SAMPLES:
            self.samples_dropped += 1
        self.samples.append(sample)

    def _run(self):
        try:
            while not self.stop_event.is_set():
                self.sample()
                self.poll_log()
                self.stop_event.wait(SAMPLE_SECONDS)
        except Exception as error:
            self.error = type(error).__name__

    def start(self):
        if self.thread is not None:
            raise ValueError("Capture already started")
        self.thread = threading.Thread(target=self._run, name="covalent-test-focus", daemon=True)
        self.thread.start()

    def stop(self):
        self.stop_event.set()
        if self.thread:
            self.thread.join(timeout=5)
        alive = bool(self.thread and self.thread.is_alive())
        if not alive:
            try:
                self.poll_log()
            except Exception as error:
                self.error = type(error).__name__
        return {"format": 1, "sourceSha256": SOURCE_SHA256, "boundedReaderSha256": READER_SHA256,
                "readerDependencySha256": READER_DEPENDENCY_SHA256,
                "rawWindowDumpsRetained": False, "rawInstrumentationTextRetained": False,
                "sampleIntervalMs": round(SAMPLE_SECONDS * 1000), "failureClass": self.error,
                "samplerAliveAfterStop": alive, "samplesDropped": self.samples_dropped,
                "failuresDropped": self.failures_dropped, "samples": [] if alive else list(self.samples),
                "failures": [] if alive else self.failures}


def run(adb, serial, phase, log, output, arguments):
    if serial not in OWNED_AVDS or phase not in {"baseline", "folder-journey"}:
        raise ValueError("Owned instrumentation phase required")
    if not arguments or arguments[-1] != "life.michaelwong.covalent.test/androidx.test.runner.AndroidJUnitRunner":
        raise ValueError("Original Covalent instrumentation runner required")
    output = Path(output)
    with output.open("x", encoding="utf-8") as stream:
        os.fchmod(stream.fileno(), 0o600)
        names = []
        admission = bounded_read([adb, "-s", serial, "emu", "avd", "name"], names.append, max_bytes=512, timeout=3)
        if admission["exitCode"] != 0 or admission["timedOut"] or admission["limitReached"] or names != [OWNED_AVDS[serial], "OK"]:
            raise ValueError("Unexpected emulator identity")
        recorder = FocusRecorder(adb, serial, log)
        process = None
        try:
            recorder.start()
            process = subprocess.Popen([adb, "-s", serial, "shell", "am", "instrument", "-w", "-r", *arguments])
            code = process.wait()
        finally:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
            try:
                result = recorder.stop()
                result.update(phase=phase, instrumentationExitCode=process.returncode if process else None,
                              instrumentationRetried=False, resultReinterpreted=False)
                json.dump(result, stream, indent=2)
                stream.write("\n")
            except Exception:
                # Diagnostics cannot replace the original child's exit/exception.
                print("Window focus evidence unavailable", file=__import__("sys").stderr)
        return code


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("adb", "serial", "phase", "log", "output"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    arguments = options.arguments[1:] if options.arguments[:1] == ["--"] else options.arguments
    def terminate(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, terminate)
    code = run(options.adb, options.serial, options.phase, options.log, options.output, arguments)
    raise SystemExit(128 - code if code < 0 else code)
