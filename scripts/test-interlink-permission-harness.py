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
import sys
import time
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

SPEC = importlib.util.spec_from_file_location("permission_qa", Path(__file__).with_name("interlink-api37-permission-test.py"))
qa = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qa)
SECRET = "private-account-and-token-must-not-be-retained"


class FakeTrace:
    def __init__(self,*args):self.events=[];self.started=False
    def start(self):self.started=True
    def mark(self,name,exit_code=None):self.events.append({"name":name,"exitCode":exit_code})
    def stop(self):return {"sourceChanged":False,"streamReady":True,"streamLiveBeforeStop":True,"readerAliveAfterStop":False,"failureClass":None,
        "events":self.events,"rawLogsRetained":False}


class LostTrace(FakeTrace):
    def stop(self):return {**super().stop(),"streamLiveBeforeStop":False}


class FrameworkTraceTests(unittest.TestCase):
    def trace(self):return qa.FrameworkTrace("adb","emulator-5572",{"system_server":1234,"surfaceflinger":5678})
    def line(self,tag,payload,pid=1234):return f"10-20 12:34:56.789 {pid:5}  1235 E {tag}: {payload}\n"

    def test_crash_metadata_omits_messages_accounts_paths_and_unrelated_processes(self):
        trace=self.trace()
        for tag,payload in (("AndroidRuntime","FATAL EXCEPTION IN SYSTEM PROCESS: "+SECRET),
                            ("AndroidRuntime","java.lang.OutOfMemoryError: "+SECRET),
                            ("am_crash","[0,1234,system_server,0,java.lang.RuntimeException,"+SECRET+"]"),
                            ("am_anr","[0,99,"+SECRET+",0,"+SECRET+"]"),
                            ("unknown",SECRET)):
            trace.consume(self.line(tag,payload))
        result=trace.snapshot()
        self.assertEqual([r["kind"] for r in result["events"]],["fatal-system-uid-process","exception-class","am_crash"])
        self.assertNotIn(SECRET,json.dumps(result));self.assertFalse(result["rawLogsRetained"])

    def test_watchdog_lmk_and_restart_are_distinct_observations(self):
        trace=self.trace()
        for tag,payload,pid in (("Watchdog","*** WATCHDOG KILLING SYSTEM PROCESS: "+SECRET,1234),
                               ("lmkd","Kill 'system_server' (1234), uid 1000: "+SECRET,2),
                               ("boot_progress_system_run","100001",9000),
                               ("libc","Fatal signal 6 (SIGABRT): "+SECRET,9000)):
            trace.consume(self.line(tag,payload,pid))
        result=trace.snapshot()
        self.assertEqual([e["kind"] for e in result["events"]],["framework-watchdog","low-memory-kill","framework-start","native-fatal-signal"])
        self.assertEqual(result["events"][1]["victimPid"],1234);self.assertNotIn(SECRET,json.dumps(result))

    def test_actual_system_fatal_prefix_does_not_invent_process_ownership(self):
        trace=self.trace()
        trace.consume(self.line("AndroidRuntime","*** FATAL EXCEPTION IN SYSTEM PROCESS: "+SECRET,9999))
        trace.consume(self.line("AndroidRuntime","java.lang.IllegalArgumentException: "+SECRET,9999))
        trace.consume(self.line("libc","Fatal signal 6 (SIGABRT): "+SECRET,8888))
        result=trace.snapshot();self.assertEqual([r["kind"] for r in result["events"]],["fatal-system-uid-process","exception-class","native-fatal-signal"])
        self.assertTrue(all("process" not in r for r in result["events"]))
        self.assertEqual(result["events"][2]["signal"],6);self.assertNotIn(SECRET,json.dumps(result))

    def test_imported_source_identity_does_not_silently_rebind_after_edits(self):
        import interlink_framework_trace as module
        trace=self.trace();original=trace.snapshot()["sourceSha256"]
        with patch.object(module.Path,"read_bytes",return_value=b"changed source"):
            changed=trace.snapshot()
        self.assertEqual(changed["sourceSha256"],original);self.assertTrue(changed["sourceChanged"])

    def test_finalizer_timeout_and_short_dead_system_names_keep_no_message(self):
        trace=self.trace()
        trace.consume(self.line("AndroidRuntime","*** FATAL EXCEPTION IN SYSTEM PROCESS: "+SECRET))
        trace.consume(self.line("AndroidRuntime","java.util.concurrent.TimeoutException: android.os.BinderProxy.finalize() timed out after 10 seconds "+SECRET))
        trace.consume(self.line("AndroidRuntime","DeadSystemException: "+SECRET))
        result=trace.snapshot();self.assertEqual(result["events"][1]["exceptionClasses"],["java.util.concurrent.TimeoutException"])
        self.assertEqual(result["events"][1]["fixedTimeoutComponent"],"android.os.BinderProxy.finalize")
        self.assertEqual(result["events"][2]["unqualifiedExceptionNames"],["DeadSystemException"])
        self.assertNotIn(SECRET,json.dumps(result))

    def test_metadata_and_process_mapping_are_bounded(self):
        trace=self.trace()
        for pid in range(1000):trace.consume(self.line("am_proc_start",f"[0,{pid},123,life.michaelwong.interlink,activity,{SECRET}]"))
        trace.consume(self.line("AndroidRuntime",SECRET*1000))
        result=trace.snapshot();self.assertEqual(len(result["events"]),256)
        self.assertEqual(result["eventsDropped"],744);self.assertLessEqual(len(trace.pids),256)
        self.assertNotIn(SECRET,json.dumps(result))

    def test_foreign_transport_and_arbitrary_phase_refused(self):
        with self.assertRaises(ValueError):qa.FrameworkTrace("adb","physical-phone",{"system_server":1,"surfaceflinger":2})
        with self.assertRaises(ValueError):self.trace().mark(SECRET)

    def test_pid_reuse_and_death_do_not_attribute_unrelated_exceptions(self):
        trace=self.trace()
        trace.consume(self.line("am_proc_start",f"[0,1234,123,{SECRET},activity,{SECRET}]"))
        trace.consume(self.line("AndroidRuntime","java.lang.RuntimeException: "+SECRET))
        trace.consume(self.line("am_proc_start","[0,9000,123,life.michaelwong.interlink,activity,private]"))
        trace.consume(self.line("am_proc_died","[0,9000,life.michaelwong.interlink,0]"))
        trace.consume(self.line("AndroidRuntime","java.lang.RuntimeException: "+SECRET,9000))
        result=trace.snapshot()
        self.assertEqual([e["kind"] for e in result["events"]],["am_proc_start","am_proc_died"])
        self.assertNotIn(1234,trace.pids);self.assertNotIn(9000,trace.pids)
        self.assertNotIn(SECRET,json.dumps(result))

    def test_actual_owned_stream_stops_without_restarting_or_replaying(self):
        import interlink_framework_trace as module
        real_popen=subprocess.Popen;created=[];trace=self.trace()
        line=self.line("AndroidRuntime","FATAL EXCEPTION IN SYSTEM PROCESS: "+SECRET)
        def launch(command,**kwargs):
            self.assertEqual(command,trace.command);self.assertNotIn("shell",command);self.assertNotIn("-c",command)
            child=real_popen([sys.executable,"-u","-c","import time;print("+repr(line.rstrip())+",flush=True);time.sleep(30)"],**kwargs)
            created.append(child);return child
        with patch.object(module.subprocess,"Popen",side_effect=launch):
            try:
                trace.start();deadline=time.monotonic()+5
                while not trace.snapshot()["linesExamined"] and time.monotonic()<deadline:time.sleep(0.01)
                self.assertIsNone(created[0].poll());self.assertTrue(trace.snapshot()["readerAlive"])
                trace.mark("input-tap-started");trace.mark("input-tap-returned",20)
                result=trace.stop();self.assertTrue(result["streamLiveBeforeStop"])
                self.assertFalse(result["readerAliveAfterStop"]);self.assertIsNotNone(created[0].poll())
                self.assertEqual(trace.stop(),result);self.assertEqual(len(created),1)
                self.assertNotIn(SECRET,json.dumps(result))
                with self.assertRaisesRegex(RuntimeError,"already started"):trace.start()
            finally:
                if created and created[0].poll() is None:created[0].kill();created[0].wait(timeout=3)

    def test_ended_stream_is_not_accepted_as_continuous_capture(self):
        import interlink_framework_trace as module
        real_popen=subprocess.Popen;trace=self.trace()
        with patch.object(module.subprocess,"Popen",side_effect=lambda command,**kw:real_popen([sys.executable,"-c","print('--------- beginning of system',flush=True)"],**kw)):
            trace.start();trace.proc.wait(timeout=5);trace.thread.join(timeout=2)
            result=trace.stop();self.assertFalse(result["streamLiveBeforeStop"])
            self.assertEqual(result["streamExitCode"],0)

    def test_unready_stream_refuses_and_retains_its_single_process(self):
        import interlink_framework_trace as module
        real_popen=subprocess.Popen;trace=self.trace()
        with patch.object(module,"START_SECONDS",0.1),patch.object(module.subprocess,"Popen",side_effect=lambda command,**kw:real_popen([sys.executable,"-c","import time;time.sleep(30)"],**kw)):
            try:
                with self.assertRaisesRegex(RuntimeError,"did not become ready"):trace.start()
                self.assertFalse(trace.snapshot()["streamReady"])
                with self.assertRaisesRegex(RuntimeError,"already started"):trace.start()
            finally:result=trace.stop()
            self.assertFalse(result["readerAliveAfterStop"]);self.assertIsNotNone(trace.proc.poll())


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
    def exercise(self, delivered, missing_failure_ui=False, cleanup_fails=False, framework_missing=False, trace_lost=False):
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
                    output = "" if framework_missing and state["failed"] else ("1234" if args[2] == "system_server" else "5678")
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
                    if framework_missing:stderr="Can't find service: input "+SECRET
                return subprocess.CompletedProcess(command, code, output, stderr)

            output = io.StringIO()
            argv = ["permission-qa", "--avd", "Interlink_QA_API37", "--serial", "emulator-5572",
                    "--evidence-dir", str(root / "evidence")]
            with patch.object(qa, "ROOT", root), patch.dict(os.environ, {"ANDROID_HOME": str(root / "sdk")}), \
                    patch.object(qa.sys, "argv", argv), patch.object(qa.subprocess, "run", adb), \
                    patch.object(qa,"FrameworkTrace",LostTrace if trace_lost else FakeTrace), \
                    contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(RuntimeError, r"shell input tap \(exit 20\)"):
                    qa.main()
            receipt = json.loads((root / "evidence/permission-recovery.json").read_text())
            self.assertEqual(receipt["result"], "failed")
            if framework_missing:self.assertEqual(receipt["frameworkAtFailure"],{"observationFailureClass":"RuntimeError"})
            else:self.assertEqual(receipt["frameworkBefore"], receipt["frameworkAtFailure"])
            self.assertEqual(receipt["identitySha256Before"], receipt["identitySha256AtFailure"])
            self.assertEqual(receipt["commandFailures"][0]["exitCode"], 20)
            self.assertNotIn(SECRET, output.getvalue() + json.dumps(receipt))
            self.assertEqual(sum(c[:3] == ("shell", "input", "tap") for c in commands), 1)
            self.assertEqual(sum(c[:3] == ("shell", "pm", "grant") for c in commands), 1)
            self.assertFalse(any("reboot" in c or "clear" in c or "force-stop" in c for c in commands))
            trace=receipt["frameworkTrace"];self.assertEqual(trace["streamLiveBeforeStop"],not trace_lost)
            tap_events=[e for e in trace["events"] if e["name"].startswith("input-tap")]
            self.assertEqual(tap_events,[{"name":"input-tap-started","exitCode":None},{"name":"input-tap-returned","exitCode":20}])
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

    def test_missing_framework_preserves_primary_failure_and_trace(self):
        receipt=self.exercise(delivered=False,missing_failure_ui=True,framework_missing=True)
        self.assertEqual(receipt["commandFailures"][0]["signals"],["missing-service"])

    def test_lost_trace_does_not_mask_or_replay_primary_failed_tap(self):
        receipt=self.exercise(delivered=False,trace_lost=True)
        self.assertEqual(receipt["cleanupFailures"],[{"action":"retain continuous framework trace","failureClass":"TraceUnavailable"}])
        self.assertEqual(len(receipt["commandFailures"]),1)

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
