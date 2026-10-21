#!/usr/bin/env python3
"""ADB cleanup must prove absence without removing a changed or foreign mapping."""
import importlib.util
from pathlib import Path
import subprocess
import unittest

SPEC = importlib.util.spec_from_file_location("reboot_qa", Path(__file__).with_name("interlink-api37-reboot-test.py"))
qa = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(qa)


class ForwardCleanupTests(unittest.TestCase):
    def run_case(self, inventories, remove_error=False):
        calls = []
        observations = iter(inventories)
        def run(*args):
            calls.append(args)
            if args == ("forward", "--list"):
                value = next(observations)
                if isinstance(value, Exception):
                    raise value
                return value
            self.assertEqual(("forward", "--remove", "tcp:39001"), args)
            if remove_error:
                raise qa.Refused("Synthetic removal error")
            return ""
        result = qa.remove_owned_forwards(run, {18887: 39001})
        return result, calls

    def test_reboot_removed_mapping_needs_only_an_observation(self):
        result, calls = self.run_case([""])
        self.assertEqual(["already-absent"], result)
        self.assertEqual([("forward", "--list")], calls)

    def test_exact_mapping_is_removed_once_and_read_back(self):
        result, calls = self.run_case(["emulator-5592 tcp:39001 tcp:18887\n", ""])
        self.assertEqual(["removed-readback-absent"], result)
        self.assertEqual(1, calls.count(("forward", "--remove", "tcp:39001")))

    def test_changed_or_foreign_mapping_is_never_removed(self):
        for row in ["emulator-5590 tcp:39001 tcp:18887\n", "emulator-5592 tcp:39001 tcp:8080\n"]:
            with self.subTest(row=row):
                result, calls = self.run_case([row])
                self.assertEqual(["mapping-changed"], result)
                self.assertEqual([("forward", "--list")], calls)

    def test_nonzero_removal_needs_independent_absence_and_is_not_retried(self):
        result, calls = self.run_case(["emulator-5592 tcp:39001 tcp:18887\n", ""], remove_error=True)
        self.assertEqual(["absent-after-remove-error"], result)
        self.assertEqual(1, calls.count(("forward", "--remove", "tcp:39001")))

    def test_a_remaining_mapping_stays_failed(self):
        row = "emulator-5592 tcp:39001 tcp:18887\n"
        result, calls = self.run_case([row, row])
        self.assertEqual(["still-present"], result)
        self.assertEqual(1, calls.count(("forward", "--remove", "tcp:39001")))

    def test_missing_inventory_is_not_empty_inventory(self):
        for value in [subprocess.TimeoutExpired("synthetic", 1), "emulator-5592 tcp:39001\n"]:
            with self.subTest(value=type(value).__name__):
                result, calls = self.run_case([value])
                self.assertEqual(["inventory-unavailable"], result)
                self.assertEqual([("forward", "--list")], calls)


if __name__ == "__main__":
    unittest.main()
