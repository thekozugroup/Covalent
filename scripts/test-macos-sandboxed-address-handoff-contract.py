#!/usr/bin/env python3
"""Offline failures for selecting the actual Cargo integration-test binary."""

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "sandboxed_gate", Path(__file__).with_name("test-macos-sandboxed-address-handoff.py"))
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


class CargoExecutableSelection(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="covalent-gate-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.binary = self.root / "one_way_links-hash"
        self.binary.write_bytes(b"synthetic test executable\n")
        self.binary.chmod(0o700)

    def record(self, **overrides):
        record = {"reason": "compiler-artifact", "target": {"name": "one_way_links", "kind": ["test"]},
                  "executable": str(self.binary)}
        record.update(overrides)
        return json.dumps(record)

    def test_exact_artifact_selected_among_dependency_messages(self):
        messages = '\n'.join((self.record(target={"name": "dependency", "kind": ["lib"]}),
                              self.record(), json.dumps({"reason": "build-finished", "success": True})))
        self.assertEqual(GATE.test_executable(messages), self.binary)

    def test_rejects_ambiguous_artifacts(self):
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record() + '\n' + self.record())

    def test_rejects_wrong_target_kind(self):
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record(target={"name": "one_way_links", "kind": ["bin"]}))

    def test_rejects_missing_artifact(self):
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record(executable=None))

    def test_rejects_non_executable(self):
        self.binary.chmod(0o600)
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record())

    def test_rejects_symbolic_link(self):
        link = self.root / "linked-test"
        link.symlink_to(self.binary)
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record(executable=str(link)))

    def test_rejects_relative_path(self):
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record(executable=self.binary.name))

    def test_rejects_missing_file(self):
        self.binary.unlink()
        with self.assertRaises(ValueError):
            GATE.test_executable(self.record())

    def test_rejects_malformed_records(self):
        for message in ('[1]', '{', self.record(target=[]), self.record(executable=7)):
            with self.subTest(message=message), self.assertRaises(ValueError):
                GATE.test_executable(message)


if __name__ == "__main__":
    unittest.main()
