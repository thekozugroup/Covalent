#!/usr/bin/env python3
"""Real host clients and redaction/failure regressions; no Android device contacted."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

import android_instrumentation_focus as focus

SECRET = "private-account@example.test PRIVATE_TOKEN photos/secret.jpg"
RUNNER = "life.michaelwong.covalent.test/androidx.test.runner.AndroidJUnitRunner"
READ = {"bytesRead": 100, "limitReached": False, "timedOut": False,
        "exitCode": 0, "clientStopped": True, "failureClass": None}


class WindowRedaction(unittest.TestCase):
    def test_only_fixed_owner_roles_and_booleans_survive(self):
        parser = focus.WindowFields()
        for line in (
            "  mCurrentFocus=Window{123 u0 life.michaelwong.covalent/androidx.activity.ComponentActivity}",
            "  mFocusedApp=ActivityRecord{9 u0 life.michaelwong.covalent.test/androidx.test.core.app.InstrumentationActivityInvoker$EmptyActivity t8}",
            "    mShowingDream=false mDreamingLockscreen=true",
            "  mTopFocusedDisplayId=0",
            "  title=" + SECRET,
        ):
            parser.consume(line)
        self.assertEqual(parser.result(), {"currentFocus": "compose-activity", "focusedApp": "test-invoker",
            "mShowingDream": False, "mDreamingLockscreen": True, "mTopFocusedDisplayId": 0})
        self.assertNotIn(SECRET, json.dumps(parser.result()))

    def test_known_system_and_app_owners_are_classified_without_names(self):
        for component, expected in (
            ("com.android.settings/.Settings", "settings"),
            ("com.android.systemui/.SomethingPrivate", "system-ui"),
            ("com.google.android.permissioncontroller/.GrantPermissionsActivity", "permission-controller"),
            ("com.google.android.apps.nexuslauncher/.NexusLauncherActivity", "launcher"),
            ("com.android.documentsui/.files.FilesActivity", "documents"),
            ("life.michaelwong.covalent/.MainActivity", "app-main"),
            ("life.michaelwong.interlink/.DifferentActivity", "app-other"),
            ("com.private.banking/.SecretAccount", "other-redacted"),
        ):
            with self.subTest(component=component):
                self.assertEqual(focus.owner("Window{9 u0 " + component + "}"), expected)

    def test_missing_null_malformed_and_conflicting_values_remain_explicit(self):
        parser = focus.WindowFields()
        self.assertEqual(parser.result()["currentFocus"], "unreported")
        parser.consume("mCurrentFocus=null")
        self.assertEqual(parser.result()["currentFocus"], "none")
        parser.consume("mCurrentFocus=" + SECRET)
        self.assertEqual(parser.result()["currentFocus"], "ambiguous")
        parser.consume("mAwake=true"); parser.consume("mAwake=false")
        self.assertEqual(parser.result()["mAwake"], "ambiguous")
        self.assertNotIn(SECRET, json.dumps(parser.result()))

    def test_overlong_and_non_boolean_payloads_never_become_evidence(self):
        parser = focus.WindowFields()
        for line in ("mCurrentFocus=" + "x" * 9000, "mAwake=true" + SECRET, "mTopFocusedDisplayId=-1"):
            parser.consume(line)
        self.assertEqual(parser.result(), {"currentFocus": "unreported", "focusedApp": "unreported"})


class Recorder(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.log = self.base / "instrumentation.log"; self.log.touch()
        self.recorder = focus.FocusRecorder("unused", "emulator-5622", self.log)

    def append(self, text):
        with self.log.open("ab") as stream:
            stream.write(text.encode())
        self.recorder.poll_log()

    def test_failure_freezes_preceding_metadata_and_never_stack_text(self):
        self.recorder.samples.append({"currentFocus": "settings"})
        self.append("INSTRUMENTATION_STATUS: current=25\nINSTRUMENTATION_STATUS_CODE: 1\n")
        self.append("INSTRUMENTATION_STATUS: stack=androidx.test.espresso.base.RootViewPicker$RootViewWithoutFocusException: " + SECRET + "\n")
        self.append("INSTRUMENTATION_STATUS_CODE: -2\n")
        self.recorder.samples.append({"currentFocus": "launcher"})
        event = self.recorder.failures[0]
        self.assertEqual((event["testOrdinal"], event["statusCode"], event["rootWithoutFocus"]), (25, -2, True))
        self.assertEqual(event["precedingSamples"], [{"currentFocus": "settings"}])
        self.assertNotIn(SECRET, json.dumps(self.recorder.stop()))

    def test_split_lines_and_overlong_line_suffix_cannot_forge_a_failure(self):
        self.append("INSTRUMENTATION_STATUS: cur")
        self.append("rent=25\nINSTRUMENTATION_STATUS_CODE: 1\n")
        self.append("x" * (focus.MAX_LINE + 1))
        self.append("INSTRUMENTATION_STATUS_CODE: -2\n")
        self.assertEqual(self.recorder.failures, [])
        self.append("INSTRUMENTATION_STATUS_CODE: -2\n")
        self.assertEqual(self.recorder.failures[0]["testOrdinal"], 25)

    def test_missing_ordinal_cannot_reuse_a_previous_test_identity(self):
        self.append("INSTRUMENTATION_STATUS: current=25\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS_CODE: 0\n")
        self.append("INSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS_CODE: -2\n")
        self.assertIsNone(self.recorder.failures[0]["testOrdinal"])

    def test_failure_and_history_counts_are_bounded(self):
        for n in range(400):
            self.recorder.samples.append({"ordinal": n})
            self.recorder.consume_status("INSTRUMENTATION_STATUS_CODE: -2")
        result = self.recorder.stop()
        self.assertEqual(len(result["samples"]), focus.MAX_SAMPLES)
        self.assertEqual(len(result["failures"]), focus.MAX_FAILURES)
        self.assertEqual(result["failuresDropped"], 400 - focus.MAX_FAILURES)
        self.assertTrue(all(len(f["precedingSamples"]) <= focus.FAILURE_HISTORY for f in result["failures"]))

    def test_log_truncation_replacement_symlink_and_read_limit_are_refused(self):
        self.append("some existing text\n")
        self.log.write_text("")
        with self.assertRaisesRegex(ValueError, "changed"):
            self.recorder.poll_log()
        self.log.unlink(); self.log.write_text("x" * (focus.MAX_LOG_CHUNK + 1))
        recorder = focus.FocusRecorder("unused", "emulator-5622", self.log)
        with self.assertRaisesRegex(ValueError, "limit"):
            recorder.poll_log()
        self.log.unlink(); self.log.symlink_to(self.base / "missing")
        with self.assertRaises(OSError):
            recorder.poll_log()

    def test_incomplete_command_cannot_supply_partial_window_fields(self):
        def read(_command, consume, **_bounds):
            consume("mCurrentFocus=Window{9 u0 com.android.settings/.Settings}")
            return {**READ, "timedOut": True}
        with patch.object(focus, "bounded_read", side_effect=read):
            self.recorder.sample()
        self.assertEqual(self.recorder.samples[0]["window"]["currentFocus"], "unavailable")
        self.assertTrue(self.recorder.samples[0]["read"]["timedOut"])

    def test_raw_sample_cap_and_owned_client_stop_are_observed(self):
        self.recorder.adb = sys.executable
        # A real local child emits an error; no adb/device is involved.
        self.recorder.sample()
        self.assertTrue(self.recorder.samples[0]["read"]["clientStopped"])
        self.assertEqual(self.recorder.samples[0]["window"]["currentFocus"], "unavailable")

    def test_stop_joins_sampler_and_preserves_its_failure(self):
        with patch.object(self.recorder, "sample", side_effect=OSError(SECRET)):
            self.recorder.start()
            result = self.recorder.stop()
        self.assertFalse(result["samplerAliveAfterStop"])
        self.assertNotIn(SECRET, json.dumps(result))


class Wrapper(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(); self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.log = self.base / "phase.log"; self.log.touch()
        self.output = self.base / "focus.json"

    def test_unknown_serial_phase_runner_and_existing_output_do_not_start_clients(self):
        cases = (("Solstice", "baseline", [RUNNER]), ("emulator-5608", "baseline", [RUNNER]),
                 ("emulator-5622", "unknown", [RUNNER]), ("emulator-5622", "baseline", ["other/Runner"]))
        with patch.object(focus, "bounded_read", side_effect=AssertionError("Unexpected contact")), \
             patch.object(focus.subprocess, "Popen", side_effect=AssertionError("Unexpected child")):
            for serial, phase, args in cases:
                with self.subTest(serial=serial, phase=phase), self.assertRaises(ValueError):
                    focus.run("adb", serial, phase, self.log, self.output, args)
            self.output.touch()
            with self.assertRaises(FileExistsError):
                focus.run("adb", "emulator-5622", "baseline", self.log, self.output, [RUNNER])

    def run_mocked(self, code, diagnostic_failure=False):
        def admission(_command, consume, **_bounds):
            consume("Interlink_Back_Focus_API37"); consume("OK"); return dict(READ)
        process = Mock(); process.wait.return_value = code; process.poll.return_value = code; process.returncode = code
        recorder = Mock(); recorder.stop.return_value = {"rawWindowDumpsRetained": False}
        with patch.object(focus, "bounded_read", side_effect=admission), \
             patch.object(focus.subprocess, "Popen", return_value=process) as popen, \
             patch.object(focus, "FocusRecorder", return_value=recorder), \
             patch.object(focus.json, "dump", side_effect=OSError(SECRET) if diagnostic_failure else None), \
             contextlib.redirect_stderr(io.StringIO()):
            result = focus.run("adb", "emulator-5622", "folder-journey", self.log, self.output, ["-e", "notClass", "original", RUNNER])
        self.assertEqual(result, code)
        popen.assert_called_once_with(["adb", "-s", "emulator-5622", "shell", "am", "instrument", "-w", "-r",
                                      "-e", "notClass", "original", RUNNER])
        process.terminate.assert_not_called(); recorder.stop.assert_called_once()

    def test_positive_and_negative_child_results_are_not_reinterpreted(self):
        for code in (0, 17, -15):
            with self.subTest(code=code):
                if self.output.exists(): self.output.unlink()
                self.run_mocked(code)

    def test_diagnostic_write_failure_cannot_replace_the_original_result(self):
        self.run_mocked(17, diagnostic_failure=True)

    def test_wrong_avd_name_is_refused_without_instrumentation(self):
        def wrong(_command, consume, **_bounds):
            consume("ForeignAVD"); consume("OK"); return dict(READ)
        with patch.object(focus, "bounded_read", side_effect=wrong), \
             patch.object(focus.subprocess, "Popen", side_effect=AssertionError("Unexpected instrumentation")):
            with self.assertRaisesRegex(ValueError, "identity"):
                focus.run("adb", "emulator-5622", "baseline", self.log, self.output, [RUNNER])


if __name__ == "__main__":
    unittest.main()
