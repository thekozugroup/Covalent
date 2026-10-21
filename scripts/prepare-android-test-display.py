#!/usr/bin/env python3
"""Prepare one owned emulator's UI before tests; never repairs a running test."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import time

from android_instrumentation_focus import OWNED_AVDS, read_window, SOURCE_SHA256 as FOCUS_SHA256
from interlink_native_reports import bounded_read, SOURCE_SHA256 as READER_SHA256

SOURCE_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
ACTIONS = (("wake", ("input", "keyevent", "KEYCODE_WAKEUP")),
           ("dismiss-keyguard", ("wm", "dismiss-keyguard")),
           ("collapse-panels", ("cmd", "statusbar", "collapse")))
READY_OWNERS = frozenset(("launcher", "settings", "app-main", "app-other", "compose-activity",
                          "test-invoker", "test-other"))
MAX_SAMPLES, DEADLINE_SECONDS, INTERVAL_SECONDS = 40, 10, .25


def complete(read):
    return read["exitCode"] == 0 and read["clientStopped"] and not any(
        read[key] for key in ("timedOut", "limitReached", "failureClass"))


def ready(observation):
    state = observation["window"]
    return (complete(observation["read"]) and state.get("currentFocus") in READY_OWNERS
            and state.get("focusedApp") == state.get("currentFocus")
            and state.get("mTopFocusedDisplayId") == 0
            and all(state.get(key) is True for key in ("mAwake", "mScreenOnEarly", "mScreenOnFully"))
            and all(state.get(key) is False for key in ("keyguardShowing", "keyguardInputRestricted",
                    "keyguardOccluded", "mKeyguardOccluded", "mPendingKeyguardOccluded",
                    "mShowingDream", "mDreamingLockscreen"))
            and state.get("mDisplayFrozen", False) is False)


def prepare(adb, serial, result):
    if serial not in OWNED_AVDS:
        result["failure"] = "unowned-serial"
        return False

    def scalar(arguments, expected):
        lines = []
        read = bounded_read([adb, "-s", serial, *arguments], lines.append, max_bytes=512, timeout=3)
        return complete(read) and expected(lines)

    if not scalar(["emu", "avd", "name"], lambda lines: lines == [OWNED_AVDS[serial], "OK"]):
        result["failure"] = "avd-identity"
        return False
    boot = []
    def admit_boot(lines):
        if len(lines) == 1 and re.fullmatch(r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}", lines[0]):
            boot.append(lines[0])
            return True
        return False
    if not scalar(["shell", "cat", "/proc/sys/kernel/random/boot_id"], admit_boot):
        result["failure"] = "boot-identity"
        return False
    result["bootId"] = boot[0]
    result["before"] = read_window(adb, serial)
    for name, arguments in ACTIONS:
        read = bounded_read([adb, "-s", serial, "shell", *arguments], lambda _line: None,
                            max_bytes=4096, timeout=3)
        result["actions"].append({"action": name, "read": read})
        if not complete(read):
            result["failure"] = "setup-command"
            return False
    deadline = time.monotonic() + DEADLINE_SECONDS
    previous_owner, consecutive = None, 0
    for _ in range(MAX_SAMPLES):
        if time.monotonic() >= deadline:
            break
        observation = read_window(adb, serial)
        result["observations"].append(observation)
        owner = observation["window"].get("currentFocus")
        if ready(observation) and time.monotonic() < deadline:
            consecutive = consecutive + 1 if owner == previous_owner else 1
        else:
            consecutive = 0
        previous_owner = owner
        if consecutive == 2:
            if not scalar(["shell", "cat", "/proc/sys/kernel/random/boot_id"], lambda lines: lines == boot):
                result["failure"] = "boot-changed-or-unavailable"
                return False
            result["ready"] = True
            return True
        remaining = deadline - time.monotonic()
        if remaining > 0:
            time.sleep(min(INTERVAL_SECONDS, remaining))
    result["failure"] = "display-not-ready"
    return False


def receipt():
    return {"format": 1, "sourceSha256": SOURCE_SHA256, "focusSourceSha256": FOCUS_SHA256,
            "boundedReaderSha256": READER_SHA256, "ready": False, "failure": None,
            "actions": [], "observations": [], "rawWindowDumpsRetained": False,
            "rawCommandOutputRetained": False, "instrumentationLaunched": False,
            "recoveryDuringTests": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("adb", "serial", "output"):
        parser.add_argument("--" + key, required=True)
    options = parser.parse_args()
    result = receipt()
    # Refuse links/existing evidence; no device action before this succeeds.
    with Path(options.output).open("x", encoding="utf-8") as stream:
        os.fchmod(stream.fileno(), 0o600)
        try:
            return 0 if prepare(options.adb, options.serial, result) else 1
        except Exception as error:
            result["failure"] = "collector-exception"
            result["failureClass"] = type(error).__name__
            return 1
        finally:
            json.dump(result, stream, indent=2)
            stream.write("\n")


if __name__ == "__main__":
    def terminate(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main())
