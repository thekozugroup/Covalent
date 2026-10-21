#!/usr/bin/env python3
"""Display setup admission, fail-closed states, bounds and command contracts."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("display", Path(__file__).with_name("prepare-android-test-display.py"))
display = importlib.util.module_from_spec(spec)
spec.loader.exec_module(display)
READ = {"exitCode": 0, "clientStopped": True, "timedOut": False, "limitReached": False,
        "failureClass": None, "bytesRead": 1}
BOOT = "169706f8-5a79-45cf-a7a2-99c8e8230ebe"
SECRET = "private@example.test token=secret DCIM/private.jpg"


def observation(owner="launcher"):
    return {"read": dict(READ), "window": {
        "currentFocus": owner, "focusedApp": owner, "mTopFocusedDisplayId": 0,
        "mAwake": True, "mScreenOnEarly": True, "mScreenOnFully": True,
        "keyguardShowing": False, "keyguardInputRestricted": False, "keyguardOccluded": False,
        "mKeyguardOccluded": False, "mPendingKeyguardOccluded": False,
        "mShowingDream": False, "mDreamingLockscreen": False}}


class Display(unittest.TestCase):
    def test_ready_requires_explicit_complete_consistent_safe_state(self):
        self.assertTrue(display.ready(observation()))
        baseline = observation()
        for key in baseline["window"]:
            item = copy.deepcopy(baseline); del item["window"][key]
            with self.subTest(missing=key): self.assertFalse(display.ready(item))
        for key in ("timedOut", "limitReached"):
            item = observation(); item["read"][key] = True
            self.assertFalse(display.ready(item))
        for key, value in (("exitCode", 1), ("clientStopped", False), ("failureClass", "OSError")):
            item = observation(); item["read"][key] = value
            self.assertFalse(display.ready(item))

    def test_overlays_sleep_lock_and_conflicting_state_are_not_ready(self):
        for owner in ("notification-shade", "status-bar", "system-ui", "permission-controller",
                      "none", "unreported", "unrecognized", "other-redacted", "ambiguous"):
            self.assertFalse(display.ready(observation(owner)))
        for key, value in (("keyguardShowing", True), ("keyguardInputRestricted", True),
                           ("keyguardOccluded", True), ("mAwake", False), ("mScreenOnEarly", False),
                           ("mKeyguardOccluded", True), ("mPendingKeyguardOccluded", True),
                           ("mScreenOnFully", False), ("mShowingDream", True),
                           ("mDreamingLockscreen", True), ("mDisplayFrozen", True),
                           ("mTopFocusedDisplayId", 1), ("focusedApp", "settings")):
            item = observation(); item["window"][key] = value
            with self.subTest(key=key): self.assertFalse(display.ready(item))
        for key in observation()["window"]:
            item = observation(); item["window"][key] = "ambiguous"
            self.assertFalse(display.ready(item))

    def run_case(self, *, serial="emulator-5622", name=None, boot=None, action_failure=None,
                 end_boot=None, samples=None, interrupt=False):
        calls = []
        boot_reads = 0
        def read(command, consume, **bounds):
            nonlocal boot_reads
            calls.append((command, bounds))
            if command[-3:] == ["emu", "avd", "name"]:
                for line in (name if name is not None else ["Interlink_Back_Focus_API37", "OK"]): consume(line)
            elif command[-1] == "/proc/sys/kernel/random/boot_id":
                boot_reads += 1
                consume((end_boot if end_boot is not None else BOOT) if boot_reads > 1
                        else (boot if boot is not None else BOOT))
            else:
                consume(SECRET)
                if interrupt: raise KeyboardInterrupt()
                if action_failure and command[-1] == action_failure: return {**READ, "exitCode": 1}
            return dict(READ)
        result = display.receipt()
        states = samples if samples is not None else [observation("notification-shade"), observation(), observation()]
        # No real clocks/clients or sleeps needed; the hard sample cap remains exercised.
        with patch.object(display, "bounded_read", side_effect=read), \
             patch.object(display, "read_window", side_effect=states), \
             patch.object(display.time, "sleep"):
            ok = display.prepare("owned-adb", serial, result)
        self.assertNotIn(SECRET, json.dumps(result))
        return ok, result, calls

    def test_unowned_serial_and_wrong_avd_never_run_setup_commands(self):
        ok, result, calls = self.run_case(serial="Solstice")
        self.assertFalse(ok); self.assertEqual(calls, [])
        ok, result, calls = self.run_case(name=["Other_AVD", "OK"])
        self.assertFalse(ok); self.assertEqual(len(calls), 1)

    def test_unknown_boot_never_runs_setup_commands(self):
        for boot in (SECRET, "", "not-a-boot-id"):
            ok, result, calls = self.run_case(boot=boot)
            self.assertFalse(ok); self.assertEqual(len(calls), 2)

    def test_exact_setup_commands_once_before_two_ready_samples(self):
        ok, result, calls = self.run_case()
        self.assertTrue(ok); self.assertTrue(result["ready"])
        self.assertEqual([c[0] for c in calls[2:-1]], [["owned-adb", "-s", "emulator-5622", "shell", *a]
                                                     for _, a in display.ACTIONS])
        self.assertEqual(len(result["observations"]), 2)
        self.assertTrue(all(c[1]["timeout"] == 3 for c in calls))
        self.assertFalse(result["instrumentationLaunched"])

    def test_setup_command_failure_stops_before_later_actions(self):
        ok, result, calls = self.run_case(action_failure="dismiss-keyguard")
        self.assertFalse(ok); self.assertEqual(result["failure"], "setup-command")
        self.assertEqual(len(result["actions"]), 2)
        self.assertEqual(result["observations"], [])
        self.assertFalse(any(c[0][-1] == "collapse" for c in calls))

    def test_readiness_is_consecutive_and_same_owner(self):
        states = [observation("notification-shade"), observation(), observation("settings"),
                  observation(), observation("notification-shade"), observation(), observation()]
        ok, result, _ = self.run_case(samples=states)
        self.assertTrue(ok); self.assertEqual(len(result["observations"]), 6)

    def test_repeated_unknown_or_incomplete_reads_hit_hard_bound(self):
        for value in (observation("unrecognized"), {**observation(), "read": {**READ, "timedOut": True}}):
            ok, result, _ = self.run_case(samples=[observation()] + [value] * display.MAX_SAMPLES)
            self.assertFalse(ok); self.assertEqual(result["failure"], "display-not-ready")
            self.assertEqual(len(result["observations"]), display.MAX_SAMPLES)

    def test_boot_change_rejects_otherwise_ready_ui(self):
        ok, result, _ = self.run_case(end_boot="269706f8-5a79-45cf-a7a2-99c8e8230ebe")
        self.assertFalse(ok); self.assertFalse(result["ready"])
        self.assertEqual(result["failure"], "boot-changed-or-unavailable")

    def test_cancellation_is_not_retried(self):
        with self.assertRaises(KeyboardInterrupt): self.run_case(interrupt=True)

    def test_elapsed_deadline_stops_before_sampling_or_recovery(self):
        with patch.object(display.time, "monotonic", side_effect=[0, 11]):
            ok, result, calls = self.run_case(samples=[observation("notification-shade")])
        self.assertFalse(ok); self.assertEqual(result["observations"], [])
        self.assertEqual(len(result["actions"]), 3)
        self.assertEqual(len(calls), 5)

    def test_existing_evidence_and_symlinks_prevent_device_actions(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "existing.json"; target.write_text("original")
            link = Path(temporary) / "link.json"; link.symlink_to(target)
            for output in (target, link):
                with patch.object(sys, "argv", ["prepare", "--adb", "unused", "--serial", "emulator-5622",
                                               "--output", str(output)]), \
                     patch.object(display, "prepare", side_effect=AssertionError("unexpected action")), \
                     self.assertRaises(FileExistsError):
                    display.main()
            self.assertEqual(target.read_text(), "original")


if __name__ == "__main__":
    unittest.main()
