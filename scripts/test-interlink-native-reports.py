#!/usr/bin/env python3
"""Independent native reports must be bounded, correlated and safe to retain."""
from datetime import datetime
import json
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

import interlink_native_reports as reports

SECRET = "private-account-token-path-thread-and-symbol"
BOOT = "12345678-1234-1234-1234-123456789012"
STAMP = "2026-10-21 13:13:58.701000000+0000"
EPOCH = int(datetime.fromisoformat(STAMP).timestamp())
FAULTS = {1775: {"tid": 1991, "process": "system_server", "signal": 6}}
TRACE = {"events": [{"kind": "native-fatal-signal", "process": "system_server",
    "sourcePid": 1775, "sourceTid": 1991, "signal": 6}]}
GOOD = ["*** *** *** *** *** *** *** ***", "Timestamp: " + STAMP,
    "pid: 1775, tid: 1991, name: " + SECRET + "  >>> system_server <<<",
    "signal 6 (SIGABRT), code -6 (SI_TKILL), fault addr --------",
    "Abort message: 'FORTIFY: " + SECRET + "'",
    "    #00 pc 0000000000079e10  /apex/com.android.runtime/lib64/bionic/libc.so (abort+160) (BuildId: " + "ab"*20 + ")",
    "    #01 pc 0000000000000123  /vendor/lib64/hw/mapper.ranchu.so (" + SECRET + "+42)"]


class ParserTests(unittest.TestCase):
    def parse(self, lines=GOOD, start=EPOCH-10, end=EPOCH+10):
        parser = reports.ReportParser(FAULTS, start, end)
        for line in lines:
            parser.consume(line)
        self.assertNotIn(SECRET, json.dumps({"events": parser.events, "counts": parser.diagnostic_counts}))
        return parser

    def test_exact_time_pid_tid_process_and_signal_retains_only_approved_details(self):
        parser = self.parse()
        self.assertEqual(parser.reports, 1)
        self.assertEqual([e["kind"] for e in parser.events], ["native-dump-header", "native-dump-signal",
            "native-abort-summary", "native-frame", "native-frame"])
        self.assertTrue(all(e["origin"] == "dropbox" and e["victimPid"] == 1775 for e in parser.events))
        self.assertEqual(parser.events[-1]["library"], "mapper.ranchu.so")
        self.assertEqual(parser.events[-2]["knownSymbol"], "abort")
        self.assertNotIn("knownSymbol", parser.events[-1])
        self.assertNotIn("/vendor/", json.dumps(parser.events))

    def test_android37_parent_header_matches_without_retaining_parent_identifier(self):
        lines = [line.replace("pid: 1775, tid:", "pid: 1775, ppid: 1234567890, tid:") for line in GOOD]
        parser = self.parse(lines)
        self.assertEqual(parser.reports, 1)
        self.assertEqual(parser.events, self.parse().events)
        self.assertNotIn("1234567890", json.dumps(parser.events))
        self.assertNotIn("ppid", json.dumps(parser.events))
        self.assertEqual(parser.diagnostic_counts["headersCorrelated"], 1)

    def test_invalid_or_extra_parent_fields_cannot_weaken_header_correlation(self):
        for parent in ("", "-1", SECRET, "1"*11, "1234, uid: 0"):
            parser = self.parse([line.replace("pid: 1775, tid:", "pid: 1775, ppid: " + parent + ", tid:") for line in GOOD])
            self.assertEqual(parser.events, [])
            self.assertEqual(parser.diagnostic_counts["headerSyntaxRejected"], 1)

    def test_live_decoder_and_report_reader_share_the_parent_header_grammar(self):
        decoder = reports.NativeDumpDecoder()
        decoder.native_faults = {1775: "system_server"}
        event = decoder._native_dump(9000, GOOD[2].replace("pid: 1775, tid:", "pid: 1775, ppid: 1234, tid:"), {})
        self.assertEqual(event, {"kind": "native-dump-header", "victimPid": 1775,
                                 "victimTid": 1991, "process": "system_server"})
        self.assertEqual(decoder._native_dump(9000, GOOD[3], {})["signal"], 6)

    def test_old_future_or_missing_timestamp_cannot_borrow_current_pid(self):
        for lines, start, end in ((GOOD, EPOCH+1, EPOCH+5), (GOOD, EPOCH-5, EPOCH-1),
                                  ([l for l in GOOD if not l.startswith("Timestamp:")], EPOCH-1, EPOCH+1)):
            self.assertEqual(self.parse(lines, start, end).events, [])

    def test_wrong_pid_tid_name_or_signal_is_not_correlated(self):
        for old, new in (("pid: 1775", "pid: 1776"), ("tid: 1991", "tid: 1992"),
                         (">>> system_server <<<", ">>> " + SECRET + " <<<"),
                         ("signal 6 (", "signal 11 (")):
            self.assertEqual(self.parse([l.replace(old, new) for l in GOOD]).events, [])

    def test_header_without_signal_has_no_admitted_events(self):
        self.assertEqual(self.parse([l for l in GOOD if not l.startswith("signal ")]).events, [])

    def test_new_or_malformed_header_resets_prior_report(self):
        for header in ("pid: malformed " + SECRET, "pid: 9999, tid: 1991, name: private  >>> system_server <<<"):
            parser = self.parse(GOOD[:4] + [header] + GOOD[4:])
            self.assertEqual([e["kind"] for e in parser.events], ["native-dump-header", "native-dump-signal"])

    def test_entry_boundary_and_invalid_timestamp_reset_context(self):
        for boundary in ("========================================", "*** *** ***", "Timestamp: " + SECRET,
                         "Timestamp: 2026-99-21 13:13:58+0000", "X" * (reports.MAX_LINE+1)):
            parser = self.parse(GOOD[:4] + [boundary] + GOOD[4:])
            self.assertEqual(len(parser.events), 2)

    def test_frames_and_events_are_bounded_even_with_repeated_reports(self):
        parser = self.parse(GOOD * 1000)
        self.assertEqual(len(parser.events), reports.MAX_EVENTS)
        self.assertEqual(parser.dropped, 5000-reports.MAX_EVENTS)
        self.assertEqual(parser.reports, 1000)

    def test_private_and_traversal_paths_are_omitted(self):
        lines = GOOD[:4] + ["    #00 pc 0000000000000001  " + path for path in
            ("/data/" + SECRET + "/libc.so", "/system/lib64/../libc.so", "/system/lib64/" + SECRET + ".so")]
        parser = self.parse(lines)
        self.assertEqual(len(parser.events), 2)
        self.assertEqual(parser.native_frames_omitted, 3)

    def test_counts_distinguish_stale_future_and_current_timestamps(self):
        for start, end, category in ((EPOCH+1, EPOCH+5, "timestampBeforeWindow"),
                                     (EPOCH-5, EPOCH-1, "timestampAfterWindow"),
                                     (EPOCH-1, EPOCH+1, "timestampInWindow")):
            parser = self.parse(start=start, end=end)
            self.assertEqual(parser.diagnostic_counts[category], 1)
            self.assertEqual(parser.diagnostic_counts["timestampLines"], 1)
            self.assertEqual(parser.diagnostic_counts["headerWithoutWindowTimestamp"], int(category!="timestampInWindow"))

    def test_counts_distinguish_malformed_time_and_strict_prefix_refusal(self):
        for replacement, category in (("Timestamp: " + SECRET, "timestampSyntaxRejected"),
                                      ("Timestamp: 2026-99-21 13:13:58+0000", "timestampParseRejected"),
                                      ("  Timestamp: " + STAMP, "timestampPrefixRejected")):
            parser = self.parse([replacement if line.startswith("Timestamp:") else line for line in GOOD])
            self.assertEqual(parser.events, [])
            self.assertEqual(parser.diagnostic_counts[category], 1)
            self.assertEqual(parser.diagnostic_counts["headerWithoutWindowTimestamp"], 1)

    def test_counts_explain_each_rejected_header_without_retaining_values(self):
        for old, new, category in (("pid: 1775", "pid: 1776", "headerPidRejected"),
                                  ("tid: 1991", "tid: 1992", "headerTidRejected"),
                                  (">>> system_server <<<", ">>> " + SECRET + " <<<", "headerProcessRejected"),
                                  ("pid: 1775", "  pid: 1775", "headerSyntaxRejected")):
            parser = self.parse([line.replace(old, new) for line in GOOD])
            self.assertEqual(parser.events, [])
            self.assertEqual(parser.diagnostic_counts[category], 1)
            self.assertEqual(parser.diagnostic_counts["headersCorrelated"], 0)

    def test_counts_distinguish_unrecognized_and_mismatched_signals(self):
        for replacement, category in (("signal " + SECRET, "signalSyntaxRejectedWithHeader"),
                                      (GOOD[3].replace("signal 6", "signal 11"), "signalRejectedWithHeader")):
            parser = self.parse(GOOD[:3]+[replacement]+GOOD[4:])
            self.assertEqual(parser.events, [])
            self.assertEqual(parser.diagnostic_counts["headersCorrelated"], 1)
            self.assertEqual(parser.diagnostic_counts[category], 1)

    def test_diagnostic_counts_saturate_without_growing_event_storage(self):
        parser = reports.ReportParser(FAULTS, EPOCH-1, EPOCH+1)
        for _ in range(65540):parser.consume("Timestamp: " + SECRET)
        self.assertEqual(parser.events, [])
        self.assertEqual(parser.diagnostic_counts["timestampLines"], 65535)
        self.assertEqual(parser.diagnostic_counts["timestampSyntaxRejected"], 65535)
        self.assertTrue(all(type(value) is int and 0 <= value <= 65535 for value in parser.diagnostic_counts.values()))
        self.assertNotIn(SECRET, json.dumps(parser.diagnostic_counts))


class BoundedClientTests(unittest.TestCase):
    def run_child(self, code, **options):
        lines = []
        result = reports.bounded_read([sys.executable, "-u", "-c", code], lines.append, **options)
        self.assertTrue(result["clientStopped"])
        return result, lines

    def test_actual_child_eof_and_final_unterminated_line(self):
        state, lines = self.run_child("import sys;sys.stdout.write('one\\nlast');sys.stdout.flush()")
        self.assertEqual(lines, ["one", "last"])
        self.assertEqual(state["exitCode"], 0)
        self.assertFalse(state["timedOut"] or state["limitReached"])

    def test_actual_nonzero_child_is_not_a_complete_report(self):
        state, _ = self.run_child("import sys;print('redacted');sys.exit(20)")
        self.assertEqual(state["exitCode"], 20)

    def test_actual_byte_limit_stops_only_owned_client(self):
        state, _ = self.run_child("import sys,time;sys.stdout.write('X'*10000);sys.stdout.flush();time.sleep(30)", max_bytes=100)
        self.assertTrue(state["limitReached"])
        self.assertLessEqual(state["bytesRead"], 101)

    def test_actual_live_silent_child_deadline_is_bounded(self):
        started = time.monotonic()
        state, lines = self.run_child("import time;time.sleep(30)", timeout=0.15)
        self.assertTrue(state["timedOut"])
        self.assertEqual(lines, [])
        self.assertLess(time.monotonic()-started, 3)

    def test_actual_eof_while_child_remains_alive_is_bounded(self):
        state, _ = self.run_child("import os,time;os.close(1);time.sleep(30)", timeout=0.15)
        self.assertTrue(state["timedOut"])

    def test_long_line_suffix_cannot_become_a_native_header(self):
        code = "import sys,time;sys.stdout.write('X'*10000);sys.stdout.flush();time.sleep(0.1);print('pid: bad');print('next')"
        state, lines = self.run_child(code)
        self.assertEqual(lines[-1], "next")
        self.assertTrue(all("pid:" not in line for line in lines))
        self.assertEqual(state["exitCode"], 0)

    def test_missing_client_error_has_no_command_or_private_message(self):
        state = reports.bounded_read(["/does/not/exist/"+SECRET], lambda line: None)
        self.assertEqual(state["failureClass"], "FileNotFoundError")
        self.assertFalse(state["clientStopped"])
        self.assertNotIn(SECRET, json.dumps(state))


class CollectorTests(unittest.TestCase):
    def collect(self, *, changed=False, partial=False, clock=None, fail_identity=False, body=GOOD, valid_format=True, report_error=False):
        commands = []
        def read(command, consume, **kwargs):
            commands.append(command)
            args = command[4:]
            state = {"bytesRead": 10, "limitReached": False, "timedOut": False,
                "exitCode": 0, "clientStopped": True, "failureClass": None}
            if args == ["cat", "/proc/sys/kernel/random/boot_id"]:
                consume("abcd1234-1234-1234-1234-123456789012" if changed and len(commands)>1 else BOOT)
                if fail_identity: state["exitCode"] = 20
            elif args == ["date", "+%s"]:
                consume(str(EPOCH+10) if clock is None else clock)
            else:
                self.assertIn(command[-1], reports.TAGS)
                if valid_format:
                    for line in ("Drop box contents: 1 entries", "Searching for: " + command[-1],
                                 "========================================" if body else "(No entries found.)"):
                        consume(line)
                for line in body: consume(line)
                if report_error: consume("*** java.io.IOException: " + SECRET)
                if partial: state.update(limitReached=True, exitCode=-15)
            return state
        with patch.object(reports, "bounded_read", side_effect=read):
            result = reports.collect("adb", "emulator-5570", BOOT, EPOCH-10, TRACE)
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertFalse(any(any(verb in c for verb in ("root", "reboot", "clear", "grant", "input")) for c in commands))
        return result, commands

    def test_two_independent_fixed_tags_same_boot_and_observed_crash_window(self):
        result, commands = self.collect()
        self.assertEqual(result["result"], "matched")
        self.assertEqual(len(commands), 5)
        self.assertEqual([a["tag"] for a in result["attempts"]], list(reports.TAGS))
        self.assertTrue(all(a["complete"] for a in result["attempts"]))
        self.assertTrue(all(a["diagnosticCounts"]["headersCorrelated"] == 1 for a in result["attempts"]))
        self.assertFalse(result["rawReportsRetained"])

    def test_incomplete_output_cannot_become_matched(self):
        result, _ = self.collect(partial=True)
        self.assertEqual(result["result"], "unavailable")
        self.assertTrue(all(not a["complete"] for a in result["attempts"]))

    def test_changed_boot_cannot_become_matched(self):
        result, _ = self.collect(changed=True)
        self.assertEqual(result["result"], "unavailable")
        self.assertEqual(result["reason"], "boot-changed")

    def test_bad_or_reversed_clock_prevents_report_commands(self):
        for clock in (SECRET, str(EPOCH-11)):
            result, commands = self.collect(clock=clock)
            self.assertEqual(result["result"], "unavailable")
            self.assertEqual(result["reason"], "clock-unavailable")
            self.assertEqual(len(commands), 2)

    def test_missing_identity_is_reported_without_private_error(self):
        result, commands = self.collect(fail_identity=True)
        self.assertEqual(result["failureClass"], "RuntimeError")
        self.assertEqual(len(commands), 1)

    def test_no_known_framework_fault_means_no_subprocess(self):
        with patch.object(reports, "bounded_read") as read:
            result = reports.collect("adb", "emulator-5570", BOOT, EPOCH, {"events": []})
        self.assertEqual(result["result"], "not-needed")
        read.assert_not_called()

    def test_physical_foreign_transport_and_bad_inputs_refused_before_read(self):
        for serial, boot, start in (("phone", BOOT, EPOCH), ("emulator-5590", BOOT, EPOCH),
                                    ("emulator-5570", SECRET, EPOCH), ("emulator-5570", BOOT, True)):
            with patch.object(reports, "bounded_read") as read:
                with self.assertRaises(ValueError): reports.collect("adb", serial, boot, start, TRACE)
            read.assert_not_called()

    def test_live_trace_failure_is_not_rewritten(self):
        before = json.dumps(TRACE, sort_keys=True)
        self.collect()
        self.assertEqual(json.dumps(TRACE, sort_keys=True), before)

    def test_empty_valid_service_response_is_distinct_from_unavailable(self):
        result, _ = self.collect(body=[])
        self.assertEqual(result["result"], "no-match")

    def test_zero_exit_permission_denial_or_missing_service_is_unavailable(self):
        for body in (["Permission Denial: " + SECRET], ["Can't find service: dropbox"], GOOD):
            result, _ = self.collect(body=body, valid_format=False)
            self.assertEqual(result["result"], "unavailable")

    def test_service_internal_read_error_invalidates_apparently_matched_report(self):
        result, _ = self.collect(report_error=True)
        self.assertEqual(result["result"], "unavailable")
        self.assertTrue(all(a["reportReadError"] for a in result["attempts"]))

    def test_source_drift_does_not_rebind_already_loaded_decoder(self):
        original_read = reports.Path.read_bytes
        for changed in ("interlink_native_reports.py", "interlink_framework_trace.py"):
            with patch.object(reports.Path, "read_bytes", lambda path: b"changed" if path.name == changed else original_read(path)):
                result, _ = self.collect()
            self.assertEqual(result["result"], "unavailable")
            self.assertEqual(result["reason"], "source-changed" if "native_reports" in changed else "decoder-changed")


if __name__ == "__main__":
    unittest.main()
