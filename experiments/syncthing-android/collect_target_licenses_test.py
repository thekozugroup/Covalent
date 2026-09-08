#!/usr/bin/env python3

from __future__ import annotations

import dataclasses
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("collect-target-licenses.py")
SPEC = importlib.util.spec_from_file_location("target_licenses", SCRIPT)
assert SPEC and SPEC.loader
target_licenses = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = target_licenses
SPEC.loader.exec_module(target_licenses)


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp.name) / "evidence"
        self.root.mkdir()
        self.root = self.root.resolve(strict=True)
        self.reports = self.root / "reports"
        self.cache = self.root / "cache"
        self.source = self.cache / "source-export"
        self.modules = self.cache / "gomodcache"
        self.source.mkdir(parents=True)
        self.modules.mkdir()
        (self.root / ".owned").write_text(
            target_licenses.OWNERSHIP_MARKER + "\n", encoding="utf-8"
        )
        self.reports.mkdir()
        self._json(
            self.reports / "collection-status.json",
            {"schemaVersion": 1, "status": "passed"},
        )
        self._json(
            self.reports / "source-export.json",
            {"commit": target_licenses.PINNED_COMMIT, "gitTree": "1" * 40},
        )
        self._json(
            self.reports / "source-after-scans.json",
            {
                "phase": "after-scans",
                "sourceTreeSha256ExcludingGeneratedAssets": "2" * 64,
            },
        )
        (self.source / "LICENSE").write_text("MPL-2.0 fixture\n", encoding="utf-8")
        (self.source / "go.mod").write_text("module fixture\n", encoding="utf-8")

    @staticmethod
    def _json(path: pathlib.Path, value: object) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value), encoding="utf-8")

    def module(self, relative: str, *, license_file: bool = True) -> pathlib.Path:
        root = self.modules / relative
        root.mkdir(parents=True)
        (root / "module.go").write_text("package module\n", encoding="utf-8")
        if license_file:
            (root / "LICENSE").write_text(f"license for {relative}\n", encoding="utf-8")
        return root

    def write_target(
        self, goarch: str, dependencies: list[tuple[str, str, pathlib.Path]]
    ) -> None:
        rows = [
            {"ImportPath": "runtime", "Standard": True},
            {
                "ImportPath": target_licenses.ROOT_PACKAGE,
                "Module": {
                    "Path": target_licenses.MAIN_MODULE,
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
        report = self.reports / f"android-{goarch}" / "go-target-deps.ndjson"
        report.parent.mkdir()
        report.write_text(
            "".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8"
        )

    def close(self) -> None:
        self.temp.cleanup()


class TargetLicenseInventoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_fifo_evidence_is_rejected_without_opening_a_writer(self) -> None:
        fifo = self.fixture.root / "fifo"
        os.mkfifo(fifo)
        for reader in (target_licenses._read_regular, target_licenses._hash_regular):
            with self.assertRaisesRegex(target_licenses.EvidenceError, "regular"):
                reader(fifo, 128)

    def test_unclassified_package_cannot_be_assumed_standard_library(self) -> None:
        self.fixture.write_target("arm64", [])
        report = self.fixture.reports / "android-arm64" / "go-target-deps.ndjson"
        with report.open("a") as stream:
            stream.write(json.dumps({"ImportPath": "example.org/unclassified"}) + "\n")
        with self.assertRaisesRegex(target_licenses.EvidenceError, "lacks module"):
            target_licenses.build_inventory(self.fixture.root)

    def test_nested_notice_and_target_difference_are_reproducible(self) -> None:
        common = self.fixture.module("example.org/common@v1.0.0")
        nested = common / "internal" / "codec"
        nested.mkdir(parents=True)
        (nested / "NOTICE.md").write_text("nested notice\n", encoding="utf-8")
        arm_only = self.fixture.module("example.org/arm@v2.0.0")
        self.fixture.write_target(
            "arm64",
            [("example.org/common", "v1.0.0", common),
             ("example.org/arm", "v2.0.0", arm_only)],
        )
        self.fixture.write_target(
            "amd64", [("example.org/common", "v1.0.0", common)]
        )

        first = target_licenses.build_inventory(self.fixture.root)
        second = target_licenses.build_inventory(self.fixture.root)

        self.assertEqual(first, second)
        self.assertEqual(first["status"], "evidence-collected-review-required")
        rows = {row["path"]: row for row in first["modules"]}
        self.assertEqual(rows["example.org/arm"]["targets"], ["arm64-v8a"])
        self.assertEqual(
            rows["example.org/common"]["targets"], ["arm64-v8a", "x86_64"]
        )
        notice = next(
            row for row in rows["example.org/common"]["licenseAndNoticeFiles"]
            if row["path"] == "internal/codec/NOTICE.md"
        )
        self.assertEqual(notice["kind"], "notice")
        self.assertEqual(
            notice["sha256"],
            "d2c0354c9769e99557d4b0a815956ef584c9d9d788e9f39a2e272229046242b1",
        )

    def test_missing_root_license_is_fixed_incomplete_status(self) -> None:
        module = self.fixture.module("example.org/missing@v1", license_file=False)
        (module / "NOTICE").write_text("notice only\n", encoding="utf-8")
        for goarch in ("arm64", "amd64"):
            self.fixture.write_target(
                goarch, [("example.org/missing", "v1", module)]
            )

        inventory = target_licenses.build_inventory(self.fixture.root)

        self.assertEqual(inventory["status"], "incomplete")
        self.assertEqual(
            inventory["missingLicenseEvidence"], ["example.org/missing@v1"]
        )

    def test_module_path_escape_is_rejected(self) -> None:
        outside = pathlib.Path(self.fixture.temp.name) / "outside"
        outside.mkdir()
        (outside / "LICENSE").write_text("outside\n", encoding="utf-8")
        for goarch in ("arm64", "amd64"):
            self.fixture.write_target(
                goarch, [("example.org/escape", "v1", outside)]
            )

        with self.assertRaisesRegex(target_licenses.EvidenceError, "escapes"):
            target_licenses.build_inventory(self.fixture.root)

    def test_symlinked_module_content_is_rejected(self) -> None:
        module = self.fixture.module("example.org/symlink@v1")
        outside = pathlib.Path(self.fixture.temp.name) / "outside-license"
        outside.write_text("outside\n", encoding="utf-8")
        (module / "NOTICE").symlink_to(outside)
        for goarch in ("arm64", "amd64"):
            self.fixture.write_target(
                goarch, [("example.org/symlink", "v1", module)]
            )

        with self.assertRaisesRegex(target_licenses.EvidenceError, "symbolic link"):
            target_licenses.build_inventory(self.fixture.root)

    def test_entry_bound_fails_before_inventory_result(self) -> None:
        module = self.fixture.module("example.org/bounded@v1")
        for index in range(4):
            (module / f"file-{index}").write_text("x", encoding="utf-8")
        for goarch in ("arm64", "amd64"):
            self.fixture.write_target(
                goarch, [("example.org/bounded", "v1", module)]
            )
        limits = dataclasses.replace(target_licenses.Limits(), module_entries=3)

        with self.assertRaisesRegex(target_licenses.EvidenceError, "entry bound"):
            target_licenses.build_inventory(self.fixture.root, limits)


if __name__ == "__main__":
    unittest.main()
