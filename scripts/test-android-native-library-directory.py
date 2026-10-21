#!/usr/bin/env python3
"""Exercise installed-package identity and immutable-helper integrity checks."""

import hashlib
import importlib.util
import pathlib
import subprocess
import sys
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

    def create_apk(self, tampered=False, manifest_override=None, tampered_abi=None):
        with zipfile.ZipFile(self.apk, "w") as archive:
            for helper, manifest in module.HELPERS.items():
                contents = {abi: ("owned ELF fixture " + helper + " " + abi).encode() for abi in module.ABI_DIRECTORIES}
                records = "".join(f"{abi} {hashlib.sha256(content).hexdigest()} {len(content)}\n" for abi,content in contents.items())
                archive.writestr("assets/" + manifest, manifest_override or records)
                for abi,content in contents.items():
                    archive.writestr("lib/" + abi + "/" + helper, b"X" * len(content) if tampered or tampered_abi==abi else content)

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

    def test_arm64_selects_its_exact_bytes_and_installer_directory(self):
        parent = "/data/app/life.michaelwong.covalent-1"
        directory, hashes = module.resolve(self.apk, ("package:" + parent + "/base.apk\n").encode(), "arm64-v8a")
        self.assertEqual(directory, parent + "/lib/arm64")
        for helper,digest in hashes.items():
            self.assertEqual(digest, hashlib.sha256(("owned ELF fixture " + helper + " arm64-v8a").encode()).hexdigest())

    def test_companion_requires_explicit_exact_installed_identity(self):
        package = "life.michaelwong.interlink"
        for parent in [f"/data/app/~~Aa_123-==/{package}-Bb_456-==", f"/data/app/{package}-1"]:
            output = f"package:{parent}/base.apk\r\n".encode()
            with self.assertRaises(ValueError):
                module.resolve(self.apk, output)
            for abi, directory_name in module.ABI_DIRECTORIES.items():
                directory, hashes = module.resolve(self.apk, output, abi, package)
                self.assertEqual(directory, parent + "/lib/" + directory_name)
                self.assertEqual(set(hashes), set(module.HELPERS))

    def test_companion_selection_rejects_other_package_and_path_injection(self):
        valid = b"package:/data/app/life.michaelwong.interlink-1/base.apk\n"
        for output in [valid.replace(b"interlink-", b"covalent-"),
                       valid.replace(b"interlink-", b"interlink.test-"),
                       valid.replace(b"/base.apk", b"/../base.apk"), valid + valid]:
            with self.subTest(output=output), self.assertRaises(ValueError):
                module.resolve(self.apk, output, "arm64-v8a", "life.michaelwong.interlink")
        for package in ["", "life.michaelwong.interlink.test", "life.*", "../interlink"]:
            with self.subTest(package=package), self.assertRaisesRegex(ValueError, "unsupported fixture package"):
                module.resolve(self.apk, valid, "arm64-v8a", package)

    def test_companion_cannot_accept_tampered_native_bytes(self):
        self.create_apk(tampered_abi="arm64-v8a")
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            module.resolve(self.apk, b"package:/data/app/life.michaelwong.interlink-1/base.apk\n",
                           "arm64-v8a", "life.michaelwong.interlink")

    def test_cli_selects_companion_without_broadening_the_default(self):
        output = b"package:/data/app/life.michaelwong.interlink-1/base.apk\n"
        command = [sys.executable, "-B", spec.origin, str(self.apk)]
        selected = subprocess.run(command + ["--abi", "arm64-v8a", "--package", "life.michaelwong.interlink"],
                                  input=output, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(selected.returncode, 0, selected.stderr)
        self.assertEqual(selected.stdout.splitlines()[0], b"/data/app/life.michaelwong.interlink-1/lib/arm64")
        default = subprocess.run(command, input=output, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(default.returncode, 1)
        self.assertFalse(default.stdout)
        unknown = subprocess.run(command + ["--package", "life.michaelwong.interlink.test"],
                                 input=output, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(unknown.returncode, 2)
        self.assertFalse(unknown.stdout)

    def test_other_abi_bytes_cannot_substitute_for_tampered_arm64(self):
        self.create_apk(tampered_abi="arm64-v8a")
        package=b"package:/data/app/life.michaelwong.covalent-1/base.apk\n"
        module.resolve(self.apk, package)
        with self.assertRaisesRegex(ValueError, "bytes differ"):
            module.resolve(self.apk, package, "arm64-v8a")

    def test_unknown_abi_or_path_fragment_is_refused(self):
        for abi in ("", "armeabi-v7a", "arm64", "../x86_64", "x86_64/../arm64"):
            with self.subTest(abi=abi),self.assertRaisesRegex(ValueError, "unsupported fixture ABI"):
                module.resolve(self.apk, b"package:/data/app/life.michaelwong.covalent-1/base.apk\n", abi)

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
