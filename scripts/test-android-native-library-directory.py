#!/usr/bin/env python3
"""Exercise installed-package identity and immutable-helper integrity checks."""

import hashlib
import importlib.util
import pathlib
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location(
    "native_directory", pathlib.Path(__file__).with_name("android-native-library-directory.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class NativeDirectoryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="covalent-native-directory-")
        self.addCleanup(self.directory.cleanup)
        self.apk = pathlib.Path(self.directory.name) / "app.apk"
        self.create_apk()

    def create_apk(self, tampered=False, manifest_override=None):
        with zipfile.ZipFile(self.apk, "w") as archive:
            for helper, manifest in module.HELPERS.items():
                content = ("owned ELF fixture " + helper).encode()
                digest = hashlib.sha256(content).hexdigest()
                records = f"arm64-v8a {digest} {len(content)}\nx86_64 {digest} {len(content)}\n"
                archive.writestr("assets/" + manifest, manifest_override or records)
                archive.writestr("lib/x86_64/" + helper, b"X" * len(content) if tampered else content)

    def test_actual_package_layouts_and_crlf_keep_exact_identity(self):
        before = self.apk.read_bytes()
        for parent in [
            "/data/app/~~Aa_123-==/life.michaelwong.covalent-Bb_456-==",
            "/data/app/life.michaelwong.covalent-1",
        ]:
            for ending in [b"\n", b"\r\n"]:
                directory, hashes = module.resolve(self.apk, f"package:{parent}/base.apk".encode() + ending)
                self.assertEqual(directory, parent + "/lib/x86_64")
                self.assertEqual(set(hashes), set(module.HELPERS))
        self.assertEqual(self.apk.read_bytes(), before)

    def test_foreign_ambiguous_or_unsafe_package_paths_fail_closed(self):
        valid = b"package:/data/app/~~123==/life.michaelwong.covalent-456==/base.apk\n"
        invalid = [
            b"", b"x" * 4097, valid + valid, valid + b"package:/data/app/split.apk\n",
            valid.replace(b"covalent-", b"covalent.test-"),
            valid.replace(b"base.apk", b"../base.apk"),
            valid.replace(b"/data/app/", b"/sdcard/"),
            valid.replace(b"456==", b"456$(touch BAD)"),
            valid.replace(b"456==", b"456;BAD"), b"nativeLibraryDir=/data/app/other/lib/x86_64\n",
        ]
        for value in invalid:
            with self.subTest(value=value[:150]), self.assertRaises(ValueError):
                module.resolve(self.apk, value)

    def test_installed_hash_plan_rejects_tampered_apk_bytes(self):
        self.create_apk(tampered=True)
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            module.resolve(self.apk, b"package:/data/app/life.michaelwong.covalent-1/base.apk\n")

    def test_partial_duplicate_or_oversized_manifests_are_rejected(self):
        row = "x86_64 " + "a" * 64 + " 32\n"
        for manifest in [row, row + row, "x" * 4097, row.replace(" 32", " 032")]:
            self.create_apk(manifest_override=manifest)
            with self.subTest(manifest=manifest[:150]), self.assertRaises(ValueError):
                module.resolve(self.apk, b"package:/data/app/life.michaelwong.covalent-1/base.apk\n")


if __name__ == "__main__":
    unittest.main()
