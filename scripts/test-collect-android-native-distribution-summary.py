#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import pathlib
import sys
import tempfile
import unittest
import warnings
import zipfile
from contextlib import redirect_stderr, redirect_stdout


SCRIPT = pathlib.Path(__file__).with_name("collect-android-native-distribution-summary.py")
SPEC = importlib.util.spec_from_file_location("native_distribution_summary", SCRIPT)
assert SPEC and SPEC.loader
summary = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = summary
SPEC.loader.exec_module(summary)


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory(dir="/tmp", prefix="cv-native-summary-")
        self.root = pathlib.Path(self.temp.name)
        self.records = self.root / "records"
        self.records.mkdir()
        self.payloads = {
            (component, abi): f"{component}:{abi}:native".encode()
            for component in summary.COMPONENTS
            for abi in summary.ABIS
        }
        self.ndk_payloads = {
            "source.properties": b"Pkg.Revision = 27.1.12297006\n",
            "NOTICE": b"Android NDK notice\n",
            "NOTICE.toolchain": b"LLVM notice\n",
        }
        self.record_paths = [
            self.write_record(component, abi)
            for component in summary.COMPONENTS
            for abi in summary.ABIS
        ]
        self.debug = self.root / "debug.apk"
        self.release = self.root / "release.apk"
        self.write_package(self.debug)
        self.write_package(self.release, marker=b"release")

    @staticmethod
    def descriptor(data: bytes) -> dict[str, object]:
        return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}

    def record_value(self, component: str, abi: str) -> dict[str, object]:
        begin = "crtbegin_so.o" if component == "jni" else "crtbegin_dynamic.o"
        requested = [
            {"kind": "android-crt", "path": "${NDK}/" + begin},
            {"kind": "compiler-rt-builtins", "path": "${NDK}/libclang_rt.builtins.a"},
        ]
        contributing = [
            {"kind": "android-crt", "path": "${NDK}/" + begin},
            {
                "kind": "compiler-rt-builtins",
                "path": "${NDK}/libclang_rt.builtins.a",
                "archiveMember": "fixture.o",
            },
        ]
        return {
            "schemaVersion": 1,
            "status": "link-inputs-classified-review-required",
            "component": component,
            "abi": abi,
            "ndk": {
                "revision": summary.EXPECTED_NDK,
                "files": {
                    name: self.descriptor(data) for name, data in self.ndk_payloads.items()
                },
            },
            "dynamicLibraries": ["libc.so"] if component == "guardian" else ["libc.so", "libdl.so"],
            "requestedNdkInputs": requested,
            "contributingNdkInputs": contributing,
            "evidence": {"binary": self.descriptor(self.payloads[(component, abi)])},
        }

    def write_record(self, component: str, abi: str) -> pathlib.Path:
        path = self.records / f"{component}-{abi}.json"
        path.write_text(json.dumps(self.record_value(component, abi), sort_keys=True) + "\n")
        return path

    def notice_fixture(self) -> tuple[dict[str, object], dict[str, bytes]]:
        files = {
            "THIRD-PARTY-NOTICES.txt": b"Readable third-party notices\n",
            "toolchain/NOTICE": self.ndk_payloads["NOTICE"],
            "toolchain/NOTICE.toolchain": self.ndk_payloads["NOTICE.toolchain"],
            "modules/0000/LICENSE": b"module license\n",
            "guardian/engine-guardian.c": b"guardian source\n",
            "guardian/Covalent-LICENSE.txt": b"guardian license\n",
        }
        manifest: dict[str, object] = {
            "schemaVersion": 1,
            "status": "texts-collected-review-required",
            "combinedNotice": {
                "bundlePath": "THIRD-PARTY-NOTICES.txt",
                **self.descriptor(files["THIRD-PARTY-NOTICES.txt"]),
            },
            "modules": [{
                "files": [{
                    "bundlePath": "modules/0000/LICENSE",
                    **self.descriptor(files["modules/0000/LICENSE"]),
                }],
            }],
            "guardian": {
                "sourceBundlePath": "guardian/engine-guardian.c",
                "sourceSha256": hashlib.sha256(files["guardian/engine-guardian.c"]).hexdigest(),
                "licenseBundlePath": "guardian/Covalent-LICENSE.txt",
                "licenseSha256": hashlib.sha256(files["guardian/Covalent-LICENSE.txt"]).hexdigest(),
            },
            "toolchainNotices": [
                {
                    "sourceName": name,
                    "bundlePath": f"toolchain/{name}",
                    **self.descriptor(files[f"toolchain/{name}"]),
                }
                for name in ("NOTICE", "NOTICE.toolchain")
            ],
        }
        return manifest, files

    def write_package(
        self,
        path: pathlib.Path,
        *,
        marker: bytes = b"debug",
        omit_notice: str | None = None,
        payloads: dict[tuple[str, str], bytes] | None = None,
    ) -> None:
        manifest, notice_files = self.notice_fixture()
        with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("assets/build-marker", marker)
            for (component, abi), data in (payloads or self.payloads).items():
                archive.writestr(f"lib/{abi}/{summary.LIBRARIES[component]}", data)
            for relative, data in notice_files.items():
                if relative != omit_notice:
                    archive.writestr("assets/sync-engine-notices/" + relative, data)
            archive.writestr(
                "assets/sync-engine-notices/manifest.json",
                json.dumps(manifest, sort_keys=True).encode(),
            )
            manifest_raw = json.dumps(manifest, sort_keys=True).encode()
            combined = notice_files["THIRD-PARTY-NOTICES.txt"]
            archive.writestr(
                "assets/sync-engine-notices-index.txt",
                "1\n"
                f"combined {hashlib.sha256(combined).hexdigest()} {len(combined)} "
                "sync-engine-notices/THIRD-PARTY-NOTICES.txt\n"
                f"manifest {hashlib.sha256(manifest_raw).hexdigest()} {len(manifest_raw)} "
                "sync-engine-notices/manifest.json\n",
            )
            for component, asset in (
                ("guardian", "engine-guardian-sha256.txt"),
                ("syncthing", "syncthing-sha256.txt"),
            ):
                rows = []
                for abi in summary.ABIS:
                    data = (payloads or self.payloads)[(component, abi)]
                    rows.append(f"{abi} {hashlib.sha256(data).hexdigest()} {len(data)}")
                archive.writestr("assets/" + asset, "\n".join(rows) + "\n")

    def close(self) -> None:
        self.temp.cleanup()


class NativeDistributionSummaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_six_records_bind_both_packages_and_emit_requested_vs_contributing(self) -> None:
        value = summary.collect(
            self.fixture.record_paths, [self.fixture.debug, self.fixture.release]
        )
        self.assertEqual(len(value["records"]), 6)
        self.assertEqual(len(value["packages"]), 2)
        self.assertEqual(value["totals"]["requestedCategories"]["android-crt"], 6)
        self.assertEqual(value["totals"]["contributingCategories"]["compiler-rt-builtins"], 6)
        self.assertEqual(value["totals"]["dynamicLibraries"], {"libc.so": 6, "libdl.so": 4})
        self.assertEqual(
            value["ndk"]["files"]["NOTICE"]["sha256"],
            hashlib.sha256(self.fixture.ndk_payloads["NOTICE"]).hexdigest(),
        )
        self.assertEqual(
            value["packages"][0]["nativeObjects"], value["packages"][1]["nativeObjects"]
        )

    def test_missing_and_duplicate_record_sets_are_rejected(self) -> None:
        with self.assertRaisesRegex(summary.SummaryError, "exactly six"):
            summary.collect(self.fixture.record_paths[:-1], [self.fixture.debug, self.fixture.release])
        duplicated = self.fixture.record_paths[:-1] + [self.fixture.record_paths[0]]
        with self.assertRaisesRegex(summary.SummaryError, "duplicated"):
            summary.collect(duplicated, [self.fixture.debug, self.fixture.release])

    def test_cross_record_ndk_notice_mismatch_is_rejected(self) -> None:
        changed = json.loads(self.fixture.record_paths[0].read_text())
        changed["ndk"]["files"]["NOTICE"]["sha256"] = "0" * 64
        self.fixture.record_paths[0].write_text(json.dumps(changed))
        with self.assertRaisesRegex(summary.SummaryError, "different NDK notice"):
            summary.collect(self.fixture.record_paths, [self.fixture.debug, self.fixture.release])

    def test_package_binary_mismatch_and_cross_package_mismatch_are_rejected(self) -> None:
        changed_payloads = dict(self.fixture.payloads)
        changed_payloads[("jni", "x86_64")] = b"different jni"
        self.fixture.write_package(self.fixture.release, payloads=changed_payloads)
        with self.assertRaisesRegex(summary.SummaryError, "differs from link evidence"):
            summary.collect(self.fixture.record_paths, [self.fixture.debug, self.fixture.release])

        changed_record = json.loads(self.fixture.record_paths[-1].read_text())
        changed_record["evidence"]["binary"]["sha256"] = "f" * 64
        self.fixture.record_paths[-1].write_text(json.dumps(changed_record))
        with self.assertRaisesRegex(summary.SummaryError, "differs from link evidence"):
            summary.collect(self.fixture.record_paths, [self.fixture.debug, self.fixture.debug])

    def test_every_declared_notice_must_be_present_and_digest_bound(self) -> None:
        self.fixture.write_package(
            self.fixture.release, omit_notice="modules/0000/LICENSE"
        )
        with self.assertRaisesRegex(summary.SummaryError, "entry is missing"):
            summary.collect(self.fixture.record_paths, [self.fixture.debug, self.fixture.release])

    def test_duplicate_package_entry_and_duplicate_json_field_fail_closed(self) -> None:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with zipfile.ZipFile(self.fixture.release, "a") as archive:
                archive.writestr("lib/arm64-v8a/libsyncthing.so", b"duplicate")
        with self.assertRaisesRegex(summary.SummaryError, "duplicate entries"):
            summary.collect(self.fixture.record_paths, [self.fixture.debug, self.fixture.release])

        self.fixture.write_package(self.fixture.release)
        self.fixture.record_paths[0].write_text(
            '{"schemaVersion":1,"schemaVersion":1}\n'
        )
        with self.assertRaisesRegex(summary.SummaryError, "duplicate field"):
            summary.collect(self.fixture.record_paths, [self.fixture.debug, self.fixture.release])

    def test_cli_writes_once_and_verifies_exact_canonical_output(self) -> None:
        output = self.fixture.root / "summary.json"
        arguments = []
        for record in self.fixture.record_paths:
            arguments.extend(("--record", str(record)))
        arguments.extend(("--debug-package", str(self.fixture.debug)))
        arguments.extend(("--release-package", str(self.fixture.release)))
        first_messages = io.StringIO()
        with redirect_stdout(first_messages), redirect_stderr(first_messages):
            self.assertEqual(summary.main([*arguments, "--output", str(output)]), 0)
        records = [
            json.loads(line.removeprefix("ANDROID_NATIVE_RECORD "))
            for line in first_messages.getvalue().splitlines()
            if line.startswith("ANDROID_NATIVE_RECORD ")
        ]
        self.assertEqual(len(records), 6)
        self.assertEqual(
            {(record["component"], record["abi"]) for record in records},
            summary.EXPECTED_RECORDS,
        )
        self.assertTrue(all(record["requestedCategories"] for record in records))
        summary_lines = [
            json.loads(line.removeprefix("ANDROID_NATIVE_SUMMARY "))
            for line in first_messages.getvalue().splitlines()
            if line.startswith("ANDROID_NATIVE_SUMMARY ")
        ]
        self.assertEqual(len(summary_lines), 1)
        self.assertEqual(summary_lines[0]["records"], 6)
        self.assertEqual(summary_lines[0]["totals"]["dynamicLibraries"]["libc.so"], 6)
        messages = io.StringIO()
        with redirect_stdout(messages), redirect_stderr(messages):
            self.assertEqual(summary.main([*arguments, "--verify-output", str(output)]), 0)
            self.assertEqual(summary.main([*arguments, "--output", str(output)]), 1)
        self.assertEqual(json.loads(output.read_text())["schemaVersion"], 1)
        output.write_text("{}\n")
        with redirect_stdout(messages), redirect_stderr(messages):
            self.assertEqual(summary.main([*arguments, "--verify-output", str(output)]), 1)


if __name__ == "__main__":
    unittest.main()
