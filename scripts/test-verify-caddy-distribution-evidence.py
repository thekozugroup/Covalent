#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import gzip
import importlib.util
import json
import io
import pathlib
import sys
import tarfile
import tempfile
import unittest


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent


def load_module(name: str, filename: str):
    specification = importlib.util.spec_from_file_location(name, SCRIPT_DIR / filename)
    assert specification and specification.loader
    module = importlib.util.module_from_spec(specification)
    sys.modules[name] = module
    specification.loader.exec_module(module)
    return module


collector_tests = load_module(
    "caddy_distribution_collector_tests_for_verifier",
    "test-collect-caddy-distribution-evidence.py",
)
verifier = load_module(
    "caddy_distribution_evidence_verifier",
    "verify-caddy-distribution-evidence.py",
)


class PackagedCaddyEvidenceVerifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = collector_tests.Fixture()
        apache = self.fixture.module("example.org/apache@v1.0.0", collector_tests.APACHE)
        mysql = self.fixture.module(
            "github.com/go-sql-driver/mysql@v1.9.3", collector_tests.MPL
        )
        (mysql / "driver.go").write_text("package mysql\n")
        report = self.fixture.report(
            "amd64",
            [
                ("example.org/apache", "v1.0.0", apache),
                ("github.com/go-sql-driver/mysql", "v1.9.3", mysql),
            ],
        )
        self.output = self.fixture.root / "evidence"
        manifest = self.fixture.collect(
            "evidence", [self.fixture.target("linux-amd64", report)]
        )
        self.old_ca_record = verifier.CA_RECORD
        self.old_consumer_record = verifier.CONSUMER_RECORD
        self.old_go_toolchain_record = verifier.GO_TOOLCHAIN_RECORD
        self.old_expected_target_records = verifier.EXPECTED_TARGET_RECORDS
        self.old_mysql_source_member_manifest_sha256 = (
            verifier.MYSQL_SOURCE_MEMBER_MANIFEST_SHA256
        )
        verifier.CA_RECORD = manifest["copiedCaCertificateBundle"]
        verifier.GO_TOOLCHAIN_RECORD = manifest["goToolchain"]
        inventory = json.loads(
            (self.output / "target-license-inventory.json").read_text()
        )
        verifier.CONSUMER_RECORD = inventory["consumer"]
        verifier.EXPECTED_TARGET_RECORDS = {
            "linux-amd64": {
                "packageCount": manifest["targets"][0]["packageCount"],
                "moduleCount": manifest["moduleCount"],
                "inventorySha256": manifest["inventory"]["sha256"],
                "noticeSha256": manifest["combinedNotice"]["sha256"],
                "binarySha256": manifest["binary"]["sha256"],
            },
            "linux-arm64": {},
        }
        verifier.MYSQL_SOURCE_MEMBER_MANIFEST_SHA256 = manifest[
            "correspondingSources"
        ][0]["archive"]["memberManifest"]["sha256"]

    def tearDown(self) -> None:
        verifier.CA_RECORD = self.old_ca_record
        verifier.CONSUMER_RECORD = self.old_consumer_record
        verifier.GO_TOOLCHAIN_RECORD = self.old_go_toolchain_record
        verifier.EXPECTED_TARGET_RECORDS = self.old_expected_target_records
        verifier.MYSQL_SOURCE_MEMBER_MANIFEST_SHA256 = (
            self.old_mysql_source_member_manifest_sha256
        )
        self.fixture.close()

    def verify(self):
        return verifier.verify(
            self.output,
            self.fixture.binary,
            self.fixture.ca_bundle,
            "linux-amd64",
        )

    def test_accepts_exact_packaged_binary_inventory_notices_and_source(self) -> None:
        result = self.verify()
        self.assertEqual(result["target"], "linux-amd64")
        self.assertEqual(result["moduleCount"], 3)
        self.assertEqual(result["unclassifiedLicenseEvidence"], [])

    def test_rejects_tampered_notice_and_unexpected_file(self) -> None:
        notice = self.output / "THIRD-PARTY-NOTICES.txt"
        original = notice.read_bytes()
        notice.chmod(0o644)
        notice.write_bytes(original + b"tamper\n")
        with self.assertRaisesRegex(verifier.VerificationError, "digest binding"):
            self.verify()

        notice.write_bytes(original)
        self.output.chmod(0o755)
        (self.output / "unexpected").write_text("not declared\n")
        with self.assertRaisesRegex(verifier.VerificationError, "file set"):
            self.verify()

    def test_rejects_symlinked_packaged_entry(self) -> None:
        notice = self.output / "THIRD-PARTY-NOTICES.txt"
        self.output.chmod(0o755)
        notice.unlink()
        notice.symlink_to(self.output / "manifest.json")
        with self.assertRaisesRegex(verifier.VerificationError, "unavailable"):
            self.verify()

    def test_rejects_self_consistent_partial_inventory(self) -> None:
        inventory_path = self.output / "target-license-inventory.json"
        manifest_path = self.output / "manifest.json"
        inventory_path.chmod(0o644)
        manifest_path.chmod(0o644)
        inventory = json.loads(inventory_path.read_text())
        inventory["modules"].pop()
        inventory["moduleCount"] = len(inventory["modules"])
        family_counts: dict[str, int] = {}
        for module in inventory["modules"]:
            for family in module["licenseFamilies"]:
                family_counts[family] = family_counts.get(family, 0) + 1
        inventory["licenseFamilyModuleCounts"] = dict(sorted(family_counts.items()))
        inventory_raw = (json.dumps(inventory, indent=2, sort_keys=True) + "\n").encode()
        inventory_path.write_bytes(inventory_raw)
        manifest = json.loads(manifest_path.read_text())
        manifest["moduleCount"] = inventory["moduleCount"]
        manifest["licenseFamilyModuleCounts"] = inventory[
            "licenseFamilyModuleCounts"
        ]
        manifest["inventory"] = {
            "bundlePath": "target-license-inventory.json",
            "bytes": len(inventory_raw),
            "sha256": hashlib.sha256(inventory_raw).hexdigest(),
        }
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")

        with self.assertRaisesRegex(verifier.VerificationError, "reviewed target"):
            self.verify()

    def test_accepts_recompressed_identical_source_members(self) -> None:
        manifest_path = self.output / "manifest.json"
        manifest_path.chmod(0o644)
        manifest = json.loads(manifest_path.read_text())
        archive_record = manifest["correspondingSources"][0]["archive"]
        archive_path = self.output / archive_record["bundlePath"]
        archive_path.chmod(0o644)
        tar_payload = gzip.decompress(archive_path.read_bytes())
        recompressed = gzip.compress(tar_payload, compresslevel=1, mtime=0)
        self.assertNotEqual(recompressed, archive_path.read_bytes())
        archive_path.write_bytes(recompressed)
        archive_record["bytes"] = len(recompressed)
        archive_record["sha256"] = hashlib.sha256(recompressed).hexdigest()
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")

        self.verify()

    def test_rejects_trailing_compressed_bytes_and_additional_gzip_streams(self) -> None:
        manifest_path = self.output / "manifest.json"
        manifest_path.chmod(0o644)
        original_manifest = json.loads(manifest_path.read_text())
        archive_record = original_manifest["correspondingSources"][0]["archive"]
        archive_path = self.output / archive_record["bundlePath"]
        archive_path.chmod(0o644)
        original_archive = archive_path.read_bytes()
        suffixes = (
            b"UNREVIEWED-TRAILER",
            gzip.compress(b"", mtime=0),
            gzip.compress(b"second stream", mtime=0),
        )
        for suffix in suffixes:
            with self.subTest(suffix=suffix[:16]):
                altered = original_archive + suffix
                archive_path.write_bytes(altered)
                manifest = json.loads(json.dumps(original_manifest))
                record = manifest["correspondingSources"][0]["archive"]
                record["bytes"] = len(altered)
                record["sha256"] = hashlib.sha256(altered).hexdigest()
                manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
                with self.assertRaisesRegex(
                    verifier.VerificationError, "compressed stream"
                ):
                    self.verify()

    def test_rejects_noncanonical_tar_data_after_end_marker(self) -> None:
        manifest_path = self.output / "manifest.json"
        manifest_path.chmod(0o644)
        manifest = json.loads(manifest_path.read_text())
        archive_record = manifest["correspondingSources"][0]["archive"]
        archive_path = self.output / archive_record["bundlePath"]
        archive_path.chmod(0o644)
        altered = gzip.compress(
            gzip.decompress(archive_path.read_bytes()) + b"\0" * 1024,
            mtime=0,
        )
        archive_path.write_bytes(altered)
        archive_record["bytes"] = len(altered)
        archive_record["sha256"] = hashlib.sha256(altered).hexdigest()
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        with self.assertRaisesRegex(verifier.VerificationError, "tar stream"):
            self.verify()

    def test_rejects_archive_path_traversal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cv-caddy-verifier-archive-") as raw:
            archive_path = pathlib.Path(raw) / "source.tar.gz"
            payload = b"outside\n"
            with tarfile.open(archive_path, "w:gz") as archive:
                entry = tarfile.TarInfo("../outside")
                entry.size = len(payload)
                archive.addfile(entry, io.BytesIO(payload))
            data = archive_path.read_bytes()
            record = {
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "entries": 1,
                "uncompressedBytes": len(payload),
            }
            with self.assertRaisesRegex(verifier.VerificationError, "path is unsafe"):
                verifier._verify_archive(archive_path, record)

    def test_rejects_wrong_target_and_binary(self) -> None:
        with self.assertRaisesRegex(verifier.VerificationError, "target binding"):
            verifier.verify(
                self.output,
                self.fixture.binary,
                self.fixture.ca_bundle,
                "linux-arm64",
            )
        self.fixture.binary.write_bytes(b"different binary")
        with self.assertRaisesRegex(verifier.VerificationError, "digest binding"):
            self.verify()

    def test_rejects_wrong_ca_bytes_and_full_manifest_binding(self) -> None:
        original_ca = self.fixture.ca_bundle.read_bytes()
        self.fixture.ca_bundle.write_bytes(b"different CA bundle")
        with self.assertRaisesRegex(verifier.VerificationError, "CA bundle provenance"):
            self.verify()

        self.fixture.ca_bundle.write_bytes(original_ca)
        manifest_path = self.output / "manifest.json"
        manifest_path.chmod(0o644)
        manifest = json.loads(manifest_path.read_text())
        manifest["copiedCaCertificateBundle"]["architecture"] = "aarch64"
        manifest["copiedCaCertificateBundle"]["bundlePath"] = "/wrong/path"
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        with self.assertRaisesRegex(verifier.VerificationError, "CA bundle provenance"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
