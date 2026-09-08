#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("collect-sync-engine-notices.py")
SPEC = importlib.util.spec_from_file_location("sync_engine_notices", SCRIPT)
assert SPEC and SPEC.loader
notices = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = notices
SPEC.loader.exec_module(notices)
REPOSITORY = SCRIPT.parent.parent.resolve(strict=True)


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory(dir="/tmp", prefix="cv-notices-")
        self.root = pathlib.Path(self.temp.name).resolve(strict=True)
        self.source = self.root / "source"
        self.cache = self.root / "gomodcache"
        self.go = self.root / "go"
        self.source.mkdir()
        self.cache.mkdir()
        self.go.mkdir()
        self._copy(
            REPOSITORY / "packaging/docker/sync-engine-notices/Syncthing-LICENSE.txt",
            self.source / "LICENSE",
        )
        self._copy(
            REPOSITORY / "packaging/docker/sync-engine-notices/Syncthing-AUTHORS.txt",
            self.source / "AUTHORS",
        )
        (self.go / "VERSION").write_text("go1.26.7\ntime fixture\n", encoding="utf-8")
        self._copy(
            REPOSITORY / "docs/licenses/sync-engine/Go-1.26.7-LICENSE.txt",
            self.go / "LICENSE",
        )
        self._copy(
            REPOSITORY / "docs/licenses/sync-engine/Go-1.26.7-PATENTS.txt",
            self.go / "PATENTS",
        )
        module_relative = pathlib.Path("example.org/!mixed@v1.2.3")
        self.module = self.cache / module_relative
        self.module.mkdir(parents=True)
        self.module_license = b"fixture dependency license text\n"
        (self.module / "LICENSE").write_bytes(self.module_license)
        self.inventory = self.root / "inventory.json"
        self.output = self.root / "bundle"
        self._write_inventory()

    @staticmethod
    def _copy(source: pathlib.Path, destination: pathlib.Path) -> None:
        destination.write_bytes(source.read_bytes())

    @staticmethod
    def _candidate(path: pathlib.Path, relative: str, kind: str) -> dict[str, object]:
        data = (path / relative).read_bytes()
        return {
            "path": relative,
            "kind": kind,
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }

    def _write_inventory(self) -> None:
        modules = [
            {
                "path": notices.MAIN_MODULE,
                "version": None,
                "status": "observed",
                "targets": ["fixture"],
                "licenseAndNoticeFiles": [
                    self._candidate(self.source, "AUTHORS", "notice"),
                    self._candidate(self.source, "LICENSE", "license"),
                ],
            },
            {
                "path": "example.org/Mixed",
                "version": "v1.2.3",
                "status": "observed",
                "targets": ["fixture"],
                "licenseAndNoticeFiles": [
                    self._candidate(self.module, "LICENSE", "license")
                ],
            },
        ]
        self.inventory.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "status": "evidence-collected-review-required",
                    "source": {
                        "version": notices.ENGINE_VERSION,
                        "commit": notices.ENGINE_COMMIT,
                    },
                    "targets": [
                        {
                            "abi": "fixture",
                            "goarch": "fixture",
                            "packageCount": 1,
                            "standardLibraryPackages": ["runtime"],
                        }
                    ],
                    "moduleCount": len(modules),
                    "modules": modules,
                    "missingLicenseEvidence": [],
                }
            ),
            encoding="utf-8",
        )

    def build(self) -> dict[str, object]:
        return notices.build_bundle(
            self.inventory,
            self.source,
            [self.cache],
            self.go,
            REPOSITORY / "packaging/sync-engine/engine-guardian.c",
            REPOSITORY / "LICENSE",
            REPOSITORY / "docs/licenses/sync-engine/OFL-1.1.txt",
            self.output,
        )

    def close(self) -> None:
        self.temp.cleanup()


class NoticeBundleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_exact_target_texts_go_and_guardian_are_bundled(self) -> None:
        manifest = self.fixture.build()

        self.assertEqual(manifest["status"], "texts-collected-review-required")
        self.assertEqual(len(manifest["modules"]), 2)
        self.assertEqual(manifest["bounds"]["files"], 8)
        self.assertEqual(
            (self.fixture.output / "modules/0001/LICENSE").read_bytes(),
            self.fixture.module_license,
        )
        self.assertEqual(
            (self.fixture.output / "go/LICENSE").read_bytes(),
            (self.fixture.go / "LICENSE").read_bytes(),
        )
        self.assertEqual(
            hashlib.sha256(
                (self.fixture.output / "guardian/engine-guardian.c").read_bytes()
            ).hexdigest(),
            notices.GUARDIAN_SHA256,
        )
        stored = json.loads((self.fixture.output / "manifest.json").read_text())
        self.assertEqual(stored, manifest)

    def test_missing_candidate_fails_without_success_manifest(self) -> None:
        (self.fixture.module / "LICENSE").unlink()

        with self.assertRaisesRegex(notices.NoticeError, "unavailable"):
            self.fixture.build()

        self.assertFalse((self.fixture.output / "manifest.json").exists())

    def test_symlink_candidate_is_rejected_without_following(self) -> None:
        outside = self.fixture.root / "outside"
        outside.write_bytes(self.fixture.module_license)
        (self.fixture.module / "LICENSE").unlink()
        (self.fixture.module / "LICENSE").symlink_to(outside)

        with self.assertRaisesRegex(notices.NoticeError, "symbolic link"):
            self.fixture.build()

        self.assertFalse((self.fixture.output / "manifest.json").exists())

    def test_inventory_digest_or_pin_change_is_rejected_before_output(self) -> None:
        document = json.loads(self.fixture.inventory.read_text())
        document["source"]["commit"] = "0" * 40
        self.fixture.inventory.write_text(json.dumps(document), encoding="utf-8")

        with self.assertRaisesRegex(notices.NoticeError, "differently pinned"):
            self.fixture.build()

        self.assertFalse(self.fixture.output.exists())

    def test_candidate_byte_count_must_match_observed_file(self) -> None:
        document = json.loads(self.fixture.inventory.read_text())
        document["modules"][1]["licenseAndNoticeFiles"][0]["bytes"] += 1
        self.fixture.inventory.write_text(json.dumps(document), encoding="utf-8")

        with self.assertRaisesRegex(notices.NoticeError, "byte count differs"):
            self.fixture.build()

        self.assertFalse((self.fixture.output / "manifest.json").exists())

    def test_module_targets_must_be_known_sorted_and_unique(self) -> None:
        document = json.loads(self.fixture.inventory.read_text())
        document["modules"][1]["targets"] = ["unknown", "fixture"]
        self.fixture.inventory.write_text(json.dumps(document), encoding="utf-8")

        with self.assertRaisesRegex(notices.NoticeError, "module inventory row"):
            self.fixture.build()

        self.assertFalse((self.fixture.output / "manifest.json").exists())


if __name__ == "__main__":
    unittest.main()
