#!/usr/bin/env python3

from __future__ import annotations

import dataclasses
import hashlib
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("collect-go-target-license-inventory.py")
SPEC = importlib.util.spec_from_file_location("go_target_licenses", SCRIPT)
assert SPEC and SPEC.loader
licenses = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = licenses
SPEC.loader.exec_module(licenses)


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory(dir="/tmp", prefix="cv-go-licenses-")
        self.root = pathlib.Path(self.temp.name).resolve(strict=True)
        self.source = self.root / "source"
        self.modules = self.root / "gomodcache"
        self.reports = self.root / "reports"
        self.source.mkdir()
        self.modules.mkdir()
        self.reports.mkdir()
        go_mod = b"module github.com/syncthing/syncthing\n"
        go_sum = b"fixture checksum evidence\n"
        (self.source / "go.mod").write_bytes(go_mod)
        (self.source / "go.sum").write_bytes(go_sum)
        (self.source / "LICENSE").write_text("MPL-2.0 fixture\n", encoding="utf-8")
        (self.source / "AUTHORS").write_text("fixture authors\n", encoding="utf-8")
        licenses.GO_MOD_SHA256 = hashlib.sha256(go_mod).hexdigest()
        licenses.GO_SUM_SHA256 = hashlib.sha256(go_sum).hexdigest()

    def module(self, escaped: str, *, license_file: bool = True) -> pathlib.Path:
        root = self.modules / escaped
        root.mkdir(parents=True)
        (root / "module.go").write_text("package module\n", encoding="utf-8")
        if license_file:
            (root / "LICENSE").write_text(f"license for {escaped}\n", encoding="utf-8")
        return root

    def report(
        self,
        name: str,
        dependencies: list[tuple[str, str, pathlib.Path]],
    ) -> pathlib.Path:
        rows = [
            {"ImportPath": "runtime", "Standard": True},
            {
                "ImportPath": licenses.ROOT_PACKAGE,
                "Module": {
                    "Path": licenses.MAIN_MODULE,
                    "Main": True,
                    "Dir": str(self.source),
                },
            },
        ]
        for index, (path, version, directory) in enumerate(dependencies):
            rows.append(
                {
                    "ImportPath": f"{path}/package{index}",
                    "Module": {"Path": path, "Version": version, "Dir": str(directory)},
                }
            )
        report = self.reports / f"{name}.ndjson"
        report.write_text(
            "".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8"
        )
        return report

    def target(self, name: str, report: pathlib.Path) -> licenses.Target:
        return licenses.Target(name, "linux", name, False, ("noupgrade",), report)

    def close(self) -> None:
        self.temp.cleanup()


class GoTargetLicenseInventoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_two_exact_targets_merge_module_evidence_reproducibly(self) -> None:
        common = self.fixture.module("example.org/common@v1.0.0")
        nested = common / "internal"
        nested.mkdir()
        (nested / "NOTICE.md").write_text("nested notice\n", encoding="utf-8")
        arm = self.fixture.module("example.org/arm@v2.0.0")
        amd_report = self.fixture.report(
            "amd64", [("example.org/common", "v1.0.0", common)]
        )
        arm_report = self.fixture.report(
            "arm64",
            [
                ("example.org/common", "v1.0.0", common),
                ("example.org/arm", "v2.0.0", arm),
            ],
        )
        targets = [
            self.fixture.target("amd64", amd_report),
            self.fixture.target("arm64", arm_report),
        ]

        first = licenses.build_inventory(self.fixture.source, self.fixture.modules, targets)
        second = licenses.build_inventory(self.fixture.source, self.fixture.modules, targets)

        self.assertEqual(first, second)
        self.assertEqual(first["status"], "evidence-collected-review-required")
        rows = {row["path"]: row for row in first["modules"]}
        self.assertEqual(rows["example.org/arm"]["targets"], ["arm64"])
        self.assertEqual(rows["example.org/common"]["targets"], ["amd64", "arm64"])
        self.assertEqual(first["targets"][0]["buildTags"], ["noupgrade"])
        notice = next(
            item
            for item in rows["example.org/common"]["licenseAndNoticeFiles"]
            if item["path"] == "internal/NOTICE.md"
        )
        self.assertEqual(notice["kind"], "notice")

    def test_missing_root_license_is_fixed_incomplete_status(self) -> None:
        missing = self.fixture.module("example.org/missing@v1", license_file=False)
        (missing / "NOTICE").write_text("notice only\n", encoding="utf-8")
        report = self.fixture.report(
            "amd64", [("example.org/missing", "v1", missing)]
        )
        inventory = licenses.build_inventory(
            self.fixture.source,
            self.fixture.modules,
            [self.fixture.target("amd64", report)],
        )
        self.assertEqual(inventory["status"], "incomplete")
        self.assertEqual(
            inventory["missingLicenseEvidence"], ["example.org/missing@v1"]
        )

    def test_wrong_source_graph_and_module_escape_are_rejected(self) -> None:
        report = self.fixture.report("amd64", [])
        (self.fixture.source / "go.sum").write_text("changed\n", encoding="utf-8")
        with self.assertRaisesRegex(licenses.EvidenceError, "go.sum differs"):
            licenses.build_inventory(
                self.fixture.source,
                self.fixture.modules,
                [self.fixture.target("amd64", report)],
            )

    def test_replacement_identity_and_exact_source_are_retained(self) -> None:
        replacement = self.fixture.module("example.org/replacement@v1.1.0")
        report = self.fixture.report("amd64", [])
        rows = [json.loads(line) for line in report.read_text().splitlines()]
        rows.append(
            {
                "ImportPath": "example.org/original/package",
                "Module": {
                    "Path": "example.org/original",
                    "Version": "v1.0.0",
                    "Dir": str(replacement),
                    "Replace": {
                        "Path": "example.org/replacement",
                        "Version": "v1.1.0",
                        "Dir": str(replacement),
                    },
                },
            }
        )
        report.write_text(
            "".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8"
        )

        inventory = licenses.build_inventory(
            self.fixture.source,
            self.fixture.modules,
            [self.fixture.target("amd64", report)],
        )

        row = next(
            row for row in inventory["modules"]
            if row["path"] == "example.org/original"
        )
        self.assertEqual(row["version"], "v1.0.0")
        self.assertEqual(
            row["replacement"],
            {"path": "example.org/replacement", "version": "v1.1.0"},
        )
        self.assertEqual(row["status"], "observed")

        (self.fixture.source / "go.sum").write_bytes(b"fixture checksum evidence\n")
        outside = self.fixture.source / "outside-module"
        outside.mkdir()
        (outside / "LICENSE").write_text("outside\n", encoding="utf-8")
        report = self.fixture.report(
            "amd64", [("example.org/escape", "v1", outside)]
        )
        with self.assertRaisesRegex(licenses.EvidenceError, "escapes"):
            licenses.build_inventory(
                self.fixture.source,
                self.fixture.modules,
                [self.fixture.target("amd64", report)],
            )

    def test_symlink_fifo_duplicate_and_bounds_fail_closed(self) -> None:
        module = self.fixture.module("example.org/unsafe@v1")
        outside = self.fixture.root / "outside"
        outside.write_text("outside\n", encoding="utf-8")
        (module / "NOTICE").symlink_to(outside)
        report = self.fixture.report(
            "amd64", [("example.org/unsafe", "v1", module)]
        )
        target = self.fixture.target("amd64", report)
        with self.assertRaisesRegex(licenses.EvidenceError, "symbolic link"):
            licenses.build_inventory(self.fixture.source, self.fixture.modules, [target])

        fifo = self.fixture.root / "report.fifo"
        os.mkfifo(fifo)
        with self.assertRaisesRegex(licenses.EvidenceError, "regular"):
            licenses._read_ndjson(fifo, licenses.Limits())

        safe = self.fixture.report("safe", [])
        duplicate = [self.fixture.target("same", safe), self.fixture.target("same", safe)]
        with self.assertRaisesRegex(licenses.EvidenceError, "sorted and unique"):
            licenses.build_inventory(self.fixture.source, self.fixture.modules, duplicate)

        limited = dataclasses.replace(licenses.Limits(), report_records=1)
        with self.assertRaisesRegex(licenses.EvidenceError, "record bound"):
            licenses.build_inventory(
                self.fixture.source,
                self.fixture.modules,
                [self.fixture.target("safe", safe)],
                limited,
            )


if __name__ == "__main__":
    unittest.main()
