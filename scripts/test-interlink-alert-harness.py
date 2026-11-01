#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("alerts", Path(__file__).with_name("interlink-api37-alert-test.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class AlertHarnessTests(unittest.TestCase):
    def proof(self):
        return {"result": "passed", "serial": "emulator-5592", "avd": "Interlink_Unlock_API37", "apkSha256": "a" * 64,
                "frameworkAfter": {"bootId": "owned-boot"}, "identitySha256After": {n: "b" * 64 for n in qa.IDENTITIES}}

    def test_admits_exact_owned_startup_and_refuses_changed_boot_apk_identity_or_result(self):
        qa.admit(self.proof(), "emulator-5592", "Interlink_Unlock_API37", "a" * 64, "owned-boot")
        for key, value in [("result", "failed"), ("apkSha256", "c" * 64), ("frameworkAfter", {}), ("identitySha256After", {}),
                           ("serial", "192.0.2.1:5555"), ("avd", "Unrelated")]:
            with self.subTest(key=key):
                proof = self.proof();proof[key] = value
                with self.assertRaises(RuntimeError):qa.admit(proof, "emulator-5592", "Interlink_Unlock_API37", "a" * 64, "owned-boot")

    def test_physical_or_mismatched_fixture_never_admits(self):
        for serial, avd in [("192.0.2.1:5555", "Interlink_Unlock_API37"), ("emulator-5592", "Covalent_API_37")]:
            with self.assertRaises(RuntimeError):qa.admit(self.proof(), serial, avd, "a" * 64, "owned-boot")

    def test_instrumentation_requires_all_tests_no_crash_skip_or_failure_and_exact_terminal_code(self):
        passing = "OK (20 tests)\nINSTRUMENTATION_CODE: -1\n"
        self.assertTrue(qa.instrument_passed(passing))
        for output in [passing.replace("20", "19"), passing.replace("-1", "0"), passing + "INSTRUMENTATION_STATUS_CODE: -3\n",
                       passing + "INSTRUMENTATION_STATUS_CODE: -2\n", passing + "shortMsg=Process crashed.\n", passing + passing,
                       "INSTRUMENTATION_CODE: -1\n", passing + "FAILURES!!!\n"]:
            with self.subTest(output=output):self.assertFalse(qa.instrument_passed(output))


if __name__ == "__main__":
    unittest.main()
