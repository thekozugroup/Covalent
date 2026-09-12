#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import json
import pathlib
import sys
import tarfile
import tempfile
import unittest
from unittest import mock


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
                "moduleSum": None,
                "recognizedLicenseTexts": ["MPL-2.0"],
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
                "moduleSum": "h1:" + "A" * 43 + "=",
                "recognizedLicenseTexts": [],
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

    def build(
        self,
        source_archive_suffix: str = ".tar.gz",
        source_patch_inputs: list[tuple[str, str, pathlib.Path, str]] | None = None,
    ) -> dict[str, object]:
        return notices.build_bundle(
            self.inventory,
            self.source,
            [self.cache],
            self.go,
            REPOSITORY / "packaging/sync-engine/engine-guardian.c",
            REPOSITORY / "LICENSE",
            REPOSITORY / "docs/licenses/sync-engine/OFL-1.1.txt",
            self.output,
            source_patch_inputs=source_patch_inputs,
            source_archive_suffix=source_archive_suffix,
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
        self.assertEqual(manifest["bounds"]["files"], 10)
        self.assertEqual(len(manifest["correspondingSources"]), 1)
        source_record = manifest["correspondingSources"][0]
        archive_path = self.fixture.output / source_record["archive"]["bundlePath"]
        self.assertEqual(
            hashlib.sha256(archive_path.read_bytes()).hexdigest(),
            source_record["archive"]["sha256"],
        )
        with tarfile.open(archive_path, "r:gz") as archive:
            self.assertEqual(
                sorted(archive.getnames()),
                ["source", "source/AUTHORS", "source/LICENSE"],
            )
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
        self.assertNotIn("toolchainNotices", manifest)
        self.assertIn(
            "Review compiler and platform runtime material separately; it is outside the Go module graph.",
            manifest["reviewRequired"],
        )
        combined = (self.fixture.output / "THIRD-PARTY-NOTICES.txt").read_bytes()
        self.assertEqual(hashlib.sha256(combined).hexdigest(), manifest["combinedNotice"]["sha256"])
        self.assertIn(self.fixture.module_license, combined)
        self.assertIn(b"Go go1.26.7 / LICENSE", combined)
        self.assertIn(b"MPL-2.0 corresponding source", combined)
        self.assertIn(source_record["archive"]["sha256"].encode(), combined)
        self.assertIn(b"github.com/syncthing/syncthing/tree/", combined)

    def test_android_asset_source_archive_keeps_gzip_bytes_under_tgz_name(self) -> None:
        manifest = self.fixture.build(source_archive_suffix=".tgz")
        archive_record = manifest["correspondingSources"][0]["archive"]
        self.assertEqual(archive_record["bundlePath"], "sources/0000-source.tgz")
        archive = self.fixture.output / archive_record["bundlePath"]
        data = archive.read_bytes()
        self.assertTrue(data.startswith(b"\x1f\x8b"))
        self.assertEqual(archive_record["bytes"], len(data))
        self.assertEqual(archive_record["sha256"], hashlib.sha256(data).hexdigest())
        with tarfile.open(archive, "r:gz") as source_tar:
            self.assertIn("source/LICENSE", source_tar.getnames())

    def test_source_patch_is_bound_to_modified_source_and_recipient_notices(self) -> None:
        patch = self.fixture.root / "keep-local-deletions.patch"
        patch.write_bytes(b"diff --git a/old b/new\n")
        digest = hashlib.sha256(patch.read_bytes()).hexdigest()
        source_patch = self.fixture.source / "covalent-patches" / patch.name
        source_patch.parent.mkdir()
        source_patch.write_bytes(patch.read_bytes())

        manifest = self.fixture.build(
            source_patch_inputs=[
                ("Covalent keep-local-deletions patch", patch.name, patch, digest)
            ]
        )

        self.assertEqual(manifest["sourceModifications"][0]["sha256"], digest)
        self.assertEqual(
            (self.fixture.output / "patches" / patch.name).read_bytes(),
            patch.read_bytes(),
        )
        main_source = manifest["correspondingSources"][0]
        self.assertEqual(main_source["sourceState"], "modified")
        self.assertEqual(main_source["modifications"][0]["sha256"], digest)
        archive_path = self.fixture.output / main_source["archive"]["bundlePath"]
        with tarfile.open(archive_path, "r:gz") as archive:
            member = archive.extractfile(
                "source/covalent-patches/keep-local-deletions.patch"
            )
            self.assertIsNotNone(member)
            self.assertEqual(member.read(), patch.read_bytes())
        combined = (self.fixture.output / "THIRD-PARTY-NOTICES.txt").read_bytes()
        self.assertIn(digest.encode(), combined)
        self.assertIn(main_source["archive"]["sha256"].encode(), combined)

    def test_exact_toolchain_notices_are_bundled_without_changing_other_callers(self) -> None:
        ndk_notice = self.fixture.root / "NOTICE"
        toolchain_notice = self.fixture.root / "NOTICE.toolchain"
        ndk_notice.write_bytes(b"Android NDK notice\n")
        toolchain_notice.write_bytes(b"LLVM toolchain notice\n")
        inputs = [
            (
                "Android NDK 27.1.12297006 / NOTICE",
                "NOTICE",
                ndk_notice,
                hashlib.sha256(ndk_notice.read_bytes()).hexdigest(),
            ),
            (
                "Android NDK 27.1.12297006 / NOTICE.toolchain",
                "NOTICE.toolchain",
                toolchain_notice,
                hashlib.sha256(toolchain_notice.read_bytes()).hexdigest(),
            ),
        ]
        manifest = notices.build_bundle(
            self.fixture.inventory,
            self.fixture.source,
            [self.fixture.cache],
            self.fixture.go,
            REPOSITORY / "packaging/sync-engine/engine-guardian.c",
            REPOSITORY / "LICENSE",
            REPOSITORY / "docs/licenses/sync-engine/OFL-1.1.txt",
            self.fixture.output,
            inputs,
        )
        self.assertEqual(
            [row["sourceName"] for row in manifest["toolchainNotices"]],
            ["NOTICE", "NOTICE.toolchain"],
        )
        combined = (self.fixture.output / "THIRD-PARTY-NOTICES.txt").read_bytes()
        self.assertIn(ndk_notice.read_bytes(), combined)
        self.assertIn(toolchain_notice.read_bytes(), combined)
        self.assertIn(b"Android NDK 27.1.12297006 / NOTICE", combined)
        self.assertNotIn(
            "Review compiler and platform runtime material separately",
            manifest["reviewRequired"],
        )

    def test_toolchain_notice_digest_name_symlink_and_bound_fail_closed(self) -> None:
        notice = self.fixture.root / "NOTICE"
        notice.write_bytes(b"Android NDK notice\n")
        arguments = (
            self.fixture.inventory,
            self.fixture.source,
            [self.fixture.cache],
            self.fixture.go,
            REPOSITORY / "packaging/sync-engine/engine-guardian.c",
            REPOSITORY / "LICENSE",
            REPOSITORY / "docs/licenses/sync-engine/OFL-1.1.txt",
        )
        with self.assertRaisesRegex(notices.NoticeError, "differs"):
            notices.build_bundle(
                *arguments,
                self.fixture.output,
                [("NDK", "NOTICE", notice, "0" * 64)],
            )

        self.fixture.output = self.fixture.root / "unsafe-name"
        with self.assertRaisesRegex(notices.NoticeError, "descriptor"):
            notices.build_bundle(
                *arguments,
                self.fixture.output,
                [("NDK", "../NOTICE", notice, hashlib.sha256(notice.read_bytes()).hexdigest())],
            )

        self.fixture.output = self.fixture.root / "linked"
        target = self.fixture.root / "target"
        target.write_bytes(notice.read_bytes())
        notice.unlink()
        notice.symlink_to(target)
        with self.assertRaisesRegex(notices.NoticeError, "unavailable|regular file"):
            notices.build_bundle(
                *arguments,
                self.fixture.output,
                [("NDK", "NOTICE", notice, hashlib.sha256(target.read_bytes()).hexdigest())],
            )

        self.fixture.output = self.fixture.root / "oversized"
        notice.unlink()
        notice.write_bytes(b"x" * 32)
        with mock.patch.object(notices, "MAX_CANDIDATE_BYTES", 16):
            with self.assertRaisesRegex(notices.NoticeError, "bounded regular file"):
                notices.build_bundle(
                    *arguments,
                    self.fixture.output,
                    [("NDK", "NOTICE", notice, hashlib.sha256(notice.read_bytes()).hexdigest())],
                )

    def test_source_archives_are_path_independent_and_mpl_dependency_is_included(self) -> None:
        mpl = b"Mozilla Public License, version 2.0\nfixture dependency\n"
        self.fixture.module_license = mpl
        (self.fixture.module / "LICENSE").write_bytes(mpl)
        (self.fixture.module / "module.go").write_text("package fixture\n")
        document = json.loads(self.fixture.inventory.read_text())
        document["modules"][1]["licenseAndNoticeFiles"] = [
            self.fixture._candidate(self.fixture.module, "LICENSE", "license")
        ]
        document["modules"][1]["recognizedLicenseTexts"] = ["MPL-2.0"]
        self.fixture.inventory.write_text(json.dumps(document), encoding="utf-8")

        first = self.fixture.build()
        second_fixture = Fixture()
        try:
            second_fixture.module_license = mpl
            (second_fixture.module / "LICENSE").write_bytes(mpl)
            (second_fixture.module / "module.go").write_text("package fixture\n")
            second_document = json.loads(second_fixture.inventory.read_text())
            second_document["modules"][1]["licenseAndNoticeFiles"] = [
                second_fixture._candidate(second_fixture.module, "LICENSE", "license")
            ]
            second_document["modules"][1]["recognizedLicenseTexts"] = ["MPL-2.0"]
            second_fixture.inventory.write_text(json.dumps(second_document), encoding="utf-8")
            second = second_fixture.build()
            self.assertEqual(
                [row["archive"]["sha256"] for row in first["correspondingSources"]],
                [row["archive"]["sha256"] for row in second["correspondingSources"]],
            )
        finally:
            second_fixture.close()
        self.assertEqual(len(first["correspondingSources"]), 2)
        dependency = first["correspondingSources"][1]
        self.assertEqual(dependency["moduleSum"], "h1:" + "A" * 43 + "=")
        self.assertIn("proxy.golang.org/example.org/!mixed", dependency["externalSourceUrl"])
        self.assertNotIn(
            b"package fixture",
            (self.fixture.output / "THIRD-PARTY-NOTICES.txt").read_bytes(),
        )

    def test_mpl_source_symlink_and_archive_bound_fail_closed(self) -> None:
        outside = self.fixture.root / "outside.go"
        outside.write_text("package outside\n")
        (self.fixture.source / "linked.go").symlink_to(outside)
        with self.assertRaisesRegex(notices.NoticeError, "symbolic link"):
            self.fixture.build()
        self.assertFalse((self.fixture.output / "manifest.json").exists())

        self.fixture.output = self.fixture.root / "bounded"
        (self.fixture.source / "linked.go").unlink()
        with mock.patch.object(notices, "MAX_SOURCE_ARCHIVE_BYTES", 512):
            with self.assertRaisesRegex(notices.NoticeError, "archive byte bound"):
                self.fixture.build()
        self.assertFalse((self.fixture.output / "manifest.json").exists())

    def test_source_mutation_during_archival_rejects_success_manifest(self) -> None:
        original = notices._source_snapshot
        calls = 0

        def mutate_before_final_snapshot(root: pathlib.Path):
            nonlocal calls
            calls += 1
            if calls == 2:
                (root / "LICENSE").write_bytes(
                    (root / "LICENSE").read_bytes() + b"changed\n"
                )
            return original(root)

        with mock.patch.object(
            notices, "_source_snapshot", side_effect=mutate_before_final_snapshot
        ):
            with self.assertRaisesRegex(notices.NoticeError, "tree changed"):
                self.fixture.build()

        self.assertFalse((self.fixture.output / "manifest.json").exists())

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

    def test_recognized_license_text_binding_cannot_be_suppressed(self) -> None:
        document = json.loads(self.fixture.inventory.read_text())
        document["modules"][0]["recognizedLicenseTexts"] = []
        self.fixture.inventory.write_text(json.dumps(document), encoding="utf-8")

        with self.assertRaisesRegex(notices.NoticeError, "license text evidence"):
            self.fixture.build()

        self.assertFalse((self.fixture.output / "manifest.json").exists())

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

    def test_replaced_module_copies_exact_replacement_source(self) -> None:
        document = json.loads(self.fixture.inventory.read_text())
        replacement_path = "example.org/Replacement"
        replacement_version = "v1.2.4"
        replacement_root = self.fixture.cache / pathlib.Path(
            f"{notices._escape_module(replacement_path)}@"
            f"{notices._escape_module(replacement_version)}"
        )
        self.fixture.module.rename(replacement_root)
        document["modules"][1]["replacement"] = {
            "path": replacement_path,
            "version": replacement_version,
        }
        document["modules"][1]["moduleSum"] = "h1:" + "B" * 43 + "="
        self.fixture.inventory.write_text(json.dumps(document), encoding="utf-8")

        manifest = self.fixture.build()

        self.assertEqual(
            manifest["modules"][1]["replacement"],
            {"path": replacement_path, "version": replacement_version},
        )
        self.assertEqual(
            (self.fixture.output / "modules/0001/LICENSE").read_bytes(),
            self.fixture.module_license,
        )
        self.assertIn(
            b"source example.org/Replacement@v1.2.4",
            (self.fixture.output / "THIRD-PARTY-NOTICES.txt").read_bytes(),
        )


if __name__ == "__main__":
    unittest.main()
