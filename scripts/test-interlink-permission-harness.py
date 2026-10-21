#!/usr/bin/env python3
"""Permission QA diagnostics must preserve failures without exposing private data."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

SPEC = importlib.util.spec_from_file_location("permission_qa", Path(__file__).with_name("interlink-api37-permission-test.py"))
qa = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qa)
SECRET = "private-account-and-token-must-not-be-retained"


class DiagnosticsTests(unittest.TestCase):
    def test_error_categories_preserve_no_raw_output_or_arguments(self):
        command = ("shell", "input", "tap", SECRET)
        stderr = "java.lang.SecurityException: " + SECRET + " requires INJECT_EVENTS"
        result = subprocess.CompletedProcess(command, 20, SECRET, stderr)
        diagnostic = qa.command_diagnostic(command, result)
        self.assertEqual(diagnostic["command"], "shell input tap")
        self.assertEqual(diagnostic["exitCode"], 20)
        self.assertEqual(diagnostic["exceptionClasses"], ["java.lang.SecurityException"])
        self.assertEqual(diagnostic["signals"], ["inject-events-permission"])
        self.assertEqual(diagnostic["stderrSha256"], hashlib.sha256(stderr.encode()).hexdigest())
        self.assertNotIn(SECRET, json.dumps(diagnostic))
        self.assertNotIn(SECRET, json.dumps(qa.command_diagnostic(("shell", SECRET), result)))

    def test_unknown_error_is_not_invented_or_leaked(self):
        result = subprocess.CompletedProcess([], 20, "", SECRET)
        diagnostic = qa.command_diagnostic(("shell", "input", "tap"), result)
        self.assertEqual(diagnostic["signals"], [])
        self.assertEqual(diagnostic["exceptionClasses"], [])
        self.assertEqual(diagnostic["exitCode"], 20)
        self.assertNotIn(SECRET, json.dumps(diagnostic))

    def test_ui_observation_is_allowlisted(self):
        tree = ET.fromstring('<hierarchy><node package="com.android.settings" text="Permissions"/>'
            f'<node package="{SECRET}" text="{SECRET}" content-desc="{SECRET}"/>'
            '<node package="com.android.permissioncontroller" text="" '
            'resource-id="com.android.permissioncontroller:id/permission_allow_button"/></hierarchy>')
        result = qa.ui_diagnostic(tree)
        self.assertEqual(result["texts"], ["Permissions"])
        self.assertEqual(result["packages"], ["com.android.permissioncontroller", "com.android.settings"])
        self.assertTrue(result["runtimeAllowVisible"])
        self.assertNotIn(SECRET, json.dumps(result))

    def test_missing_hierarchy_is_explicit(self):
        self.assertEqual(qa.ui_diagnostic(None), {"available": False})


class FailedInputJourneyTests(unittest.TestCase):
    def exercise(self, delivered, missing_failure_ui=False, cleanup_fails=False):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            apk = root / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
            apk.parent.mkdir(parents=True)
            apk.write_bytes(b"synthetic debug APK; no real device is contacted")
            digest = hashlib.sha256(apk.read_bytes()).hexdigest()
            commands = []
            state = {"installed": False, "view": "ready", "failed": False}

            def adb(command, **kwargs):
                args = tuple(command[3:])  # adb -s exact-serial ...
                commands.append(args)
                output, stderr, code = "", "", 0
                if args == ("emu", "avd", "name"):
                    output = "Interlink_QA_API37\nOK"
                elif args[:2] == ("shell", "getprop"):
                    output = "37" if args[2] == "ro.build.version.sdk" else "1"
                elif args[:3] == ("shell", "cat", "/proc/sys/kernel/random/boot_id"):
                    output = "12345678-1234-1234-1234-123456789012"
                elif args[:2] == ("shell", "pidof"):
                    output = "1234" if args[2] == "system_server" else "5678"
                elif args[:3] == ("shell", "pm", "path"):
                    output = "package:/data/app/synthetic/base.apk" if state["installed"] else ""
                elif args[0] == "install":
                    state["installed"] = True
                elif args[:2] == ("shell", "sha256sum"):
                    output = digest + "  synthetic/base.apk"
                elif args[:2] == ("shell", "getconf"):
                    output = "4096"
                elif args[:2] == ("shell", "uiautomator"):
                    if not (missing_failure_ui and state["failed"]):
                        output = "UI hierchary dumped to: " + args[3]
                elif args[:2] == ("shell", "cat"):
                    text = {"ready": "Pair your Pi to continue", "recovery": "Nearby devices access is off",
                            "settings": "Permissions"}[state["view"]]
                    package = "com.android.settings" if state["view"] == "settings" else qa.PACKAGE
                    output = f'<hierarchy><node package="{package}" text="{text}"/>'
                    if state["view"] == "recovery":
                        output += '<node text="Allow access" bounds="[1,2][20,30]"/>'
                        output += '<node text="Open settings" bounds="[10,40][200,90]"/>'
                    output += f'<node text="{SECRET}"/></hierarchy>'
                elif args[:3] == ("shell", "dumpsys", "activity"):
                    output = "life.michaelwong.covalent.node.NodeProviderService isForeground=true"
                elif args[:2] == ("shell", "run-as") and "sha256sum" in args:
                    output = "a" * 64 + "  synthetic identity"
                elif args[:3] == ("shell", "pm", "revoke"):
                    state["view"] = "recovery"
                elif args[:3] == ("shell", "pm", "grant") and cleanup_fails:
                    code, stderr = 20, "android.os.DeadObjectException " + SECRET
                elif args[:3] == ("shell", "input", "tap"):
                    state["failed"] = True
                    if delivered:
                        state["view"] = "settings"
                    code, stderr = 20, "java.lang.SecurityException: " + SECRET + " INJECT_EVENTS"
                return subprocess.CompletedProcess(command, code, output, stderr)

            output = io.StringIO()
            argv = ["permission-qa", "--avd", "Interlink_QA_API37", "--serial", "emulator-5572",
                    "--evidence-dir", str(root / "evidence")]
            with patch.object(qa, "ROOT", root), patch.dict(os.environ, {"ANDROID_HOME": str(root / "sdk")}), \
                    patch.object(qa.sys, "argv", argv), patch.object(qa.subprocess, "run", adb), \
                    contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(RuntimeError, r"shell input tap \(exit 20\)"):
                    qa.main()
            receipt = json.loads((root / "evidence/permission-recovery.json").read_text())
            self.assertEqual(receipt["result"], "failed")
            self.assertEqual(receipt["frameworkBefore"], receipt["frameworkAtFailure"])
            self.assertEqual(receipt["identitySha256Before"], receipt["identitySha256AtFailure"])
            self.assertEqual(receipt["commandFailures"][0]["exitCode"], 20)
            self.assertNotIn(SECRET, output.getvalue() + json.dumps(receipt))
            self.assertEqual(sum(c[:3] == ("shell", "input", "tap") for c in commands), 1)
            self.assertEqual(sum(c[:3] == ("shell", "pm", "grant") for c in commands), 1)
            self.assertFalse(any("reboot" in c or "clear" in c or "force-stop" in c for c in commands))
            return receipt

    def test_nonzero_tap_stays_failed_when_screen_changed(self):
        receipt = self.exercise(delivered=True)
        self.assertEqual(receipt["failureUi"]["texts"], ["Permissions"])
        self.assertEqual(receipt["failureUi"]["packages"], ["com.android.settings"])

    def test_nonzero_tap_stays_failed_when_screen_did_not_change(self):
        receipt = self.exercise(delivered=False)
        self.assertIn("Nearby devices access is off", receipt["failureUi"]["texts"])

    def test_failed_dump_does_not_reuse_old_hierarchy(self):
        receipt = self.exercise(delivered=True, missing_failure_ui=True)
        self.assertEqual(receipt["failureUi"], {"available": False})

    def test_cleanup_failure_keeps_primary_error_and_writes_receipt(self):
        receipt = self.exercise(delivered=False, cleanup_fails=True)
        self.assertEqual(receipt["cleanupFailures"], [{"action": "restore QA permission", "failureClass": "RuntimeError"}])
        self.assertEqual(receipt["commandFailures"][1]["exceptionClasses"], ["android.os.DeadObjectException"])

    def test_foreign_serial_refused_before_any_adb_command(self):
        argv = ["qa", "--avd", "Interlink_QA_API37", "--serial", "emulator-5590", "--evidence-dir", "/unused"]
        with patch.object(qa.sys, "argv", argv), patch.object(qa.subprocess, "run") as adb, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                qa.main()
        adb.assert_not_called()


if __name__ == "__main__":
    unittest.main()
