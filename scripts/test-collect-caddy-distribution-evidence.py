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


SCRIPT = pathlib.Path(__file__).with_name("collect-caddy-distribution-evidence.py")
SPEC = importlib.util.spec_from_file_location("caddy_distribution_evidence", SCRIPT)
assert SPEC and SPEC.loader
evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evidence
SPEC.loader.exec_module(evidence)


MIT = b"""MIT License

Copyright (c) Example

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
"""

APACHE = b"""Apache License
Version 2.0, January 2004
TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION
1. Definitions.
9. Accepting Warranty or Additional Liability.
"""

MPL = b"""Mozilla Public License, version 2.0
1. Definitions
Exhibit B - "Incompatible With Secondary Licenses" Notice
"""


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="cv-caddy-evidence-")
        self.root = pathlib.Path(self.temp.name).resolve(strict=True)
        self.source = self.root / "source"
        self.modules = self.root / "gomodcache"
        self.go = self.root / "go"
        self.source.mkdir()
        self.modules.mkdir()
        self.go.mkdir()
        (self.go / "VERSION").write_text("go-test\n")
        (self.go / "LICENSE").write_bytes(MIT)
        (self.go / "PATENTS").write_text("patents\n")
        (self.source / "go.mod").write_text("module covalent.local/caddy\n")
        (self.source / "go.sum").write_text("fixture sums\n")
        (self.source / "main.go").write_text("package main\nfunc main() {}\n")
        (self.source / "LICENSE").write_bytes(MIT)
        self.binary = self.root / "caddy"
        self.set_binary_arch("amd64")
        self.ca_bundle = self.root / "ca-certificates.crt"
        self.ca_bundle.write_bytes(b"fixture CA bundle")
        self.apk_installed = self.root / "apk-installed"
        self.apk_installed.write_text(
            "P:ca-certificates-bundle\n"
            "V:test-ca-r0\n"
            "A:x86_64\n"
            "L:MPL-2.0 AND MIT\n"
            "o:ca-certificates\n"
            "c:" + "a" * 40 + "\n"
            "F:etc/ssl/certs\n"
            "R:ca-certificates.crt\n\n"
        )
        self._bind_constants()

    def _bind_constants(self) -> None:
        evidence.GO_VERSION = "go-test"
        evidence.GO_MOD_SHA256 = self.digest(self.source / "go.mod")
        evidence.GO_SUM_SHA256 = self.digest(self.source / "go.sum")
        evidence.MAIN_GO_SHA256 = self.digest(self.source / "main.go")
        evidence.COVALENT_LICENSE_SHA256 = self.digest(self.source / "LICENSE")
        evidence.GO_LICENSE_SHA256 = self.digest(self.go / "LICENSE")
        evidence.GO_PATENTS_SHA256 = self.digest(self.go / "PATENTS")
        evidence.CA_VERSION = "test-ca-r0"
        evidence.CA_APORTS_COMMIT = "a" * 40
        evidence.CA_BUNDLE_BYTES = self.ca_bundle.stat().st_size
        evidence.CA_BUNDLE_SHA256 = self.digest(self.ca_bundle)

    @staticmethod
    def digest(path: pathlib.Path) -> str:
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def set_binary_arch(self, architecture: str) -> None:
        machine = {"amd64": 62, "arm64": 183}[architecture]
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = machine.to_bytes(2, "little")
        self.binary.write_bytes(header)

    def module(self, name: str, license_text: bytes) -> pathlib.Path:
        root = self.modules / name
        root.mkdir(parents=True)
        (root / "LICENSE").write_bytes(license_text)
        (root / "module.go").write_text("package module\n")
        return root

    def report(
        self, name: str, dependencies: list[tuple[str, str, pathlib.Path]]
    ) -> pathlib.Path:
        rows = [
            {"ImportPath": "runtime", "Standard": True},
            {
                "ImportPath": evidence.ROOT_PACKAGE,
                "Module": {
                    "Path": evidence.MAIN_MODULE,
                    "Main": True,
                    "Dir": str(self.source),
                },
            },
        ]
        for index, (path, version, directory) in enumerate(dependencies):
            rows.append({
                "ImportPath": f"{path}/package{index}",
                "Module": {
                    "Path": path, "Version": version, "Dir": str(directory),
                    "Sum": "h1:" + "A" * 43 + "=",
                },
            })
        path = self.root / f"{name}.ndjson"
        path.write_text("".join(json.dumps(row) + "\n" for row in rows))
        return path

    def target(self, name: str, report: pathlib.Path) -> evidence.Target:
        return evidence.Target(name, "linux", name.removeprefix("linux-"), report)

    def collect(
        self, output_name: str, targets: list[evidence.Target]
    ) -> dict:
        return evidence.collect(
            self.source, self.modules, self.go, targets, self.binary,
            self.apk_installed, self.ca_bundle, self.root / output_name,
        )

    def close(self) -> None:
        self.temp.cleanup()


class CaddyDistributionEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_each_target_is_deterministic_and_archives_mpl_source(self) -> None:
        apache = self.fixture.module("example.org/apache@v1.0.0", APACHE)
        mpl = self.fixture.module("example.org/mpl@v2.0.0", MPL)
        (mpl / "data.txt").write_text("corresponding source\n")
        amd = self.fixture.report(
            "amd64", [
                ("example.org/apache", "v1.0.0", apache),
                ("example.org/mpl", "v2.0.0", mpl),
            ]
        )
        arm = self.fixture.report(
            "arm64", [
                ("example.org/apache", "v1.0.0", apache),
                ("example.org/mpl", "v2.0.0", mpl),
            ],
        )
        target = [self.fixture.target("linux-amd64", amd)]
        first = self.fixture.collect("first", target)
        second = self.fixture.collect("second", target)

        self.assertEqual(first, second)
        self.assertEqual(first["moduleCount"], 3)
        self.assertEqual(first["unclassifiedLicenseEvidence"], [])
        self.assertEqual(len(first["correspondingSources"]), 1)
        archive = self.fixture.root / "first" / first["correspondingSources"][0]["archive"]["bundlePath"]
        with tarfile.open(archive, "r:gz") as source:
            self.assertEqual(
                source.getnames(),
                ["source", "source/LICENSE", "source/data.txt", "source/module.go"],
            )
        self.assertEqual(
            (self.fixture.root / "first" / "THIRD-PARTY-NOTICES.txt").read_bytes(),
            (self.fixture.root / "second" / "THIRD-PARTY-NOTICES.txt").read_bytes(),
        )
        self.fixture.set_binary_arch("arm64")
        arm_result = self.fixture.collect(
            "arm", [self.fixture.target("linux-arm64", arm)]
        )
        self.assertEqual(arm_result["targets"][0]["name"], "linux-arm64")
        self.assertEqual(arm_result["moduleCount"], 3)

    def test_partial_license_text_is_rejected_as_incomplete(self) -> None:
        partial = self.fixture.module(
            "example.org/partial@v1.0.0",
            b"Apache License Version 2.0, January 2004\n",
        )
        report = self.fixture.report(
            "amd64", [("example.org/partial", "v1.0.0", partial)]
        )

        with self.assertRaisesRegex(
            evidence.EvidenceError, "target license evidence is incomplete"
        ):
            self.fixture.collect(
                "partial", [self.fixture.target("linux-amd64", report)]
            )

    def test_symlinked_candidate_is_rejected_without_partial_output(self) -> None:
        module = self.fixture.module("example.org/module@v1.0.0", MIT)
        (module / "NOTICE").symlink_to(module / "LICENSE")
        report = self.fixture.report(
            "amd64", [("example.org/module", "v1.0.0", module)]
        )

        with self.assertRaisesRegex(evidence.EvidenceError, "symbolic link"):
            self.fixture.collect(
                "symlink", [self.fixture.target("linux-amd64", report)]
            )
        self.assertFalse((self.fixture.root / "symlink").exists())

    def test_load_error_and_source_escape_are_rejected(self) -> None:
        report = self.fixture.report("amd64", [])
        rows = [json.loads(line) for line in report.read_text().splitlines()]
        rows[0]["Error"] = {"Err": "fixture"}
        report.write_text("".join(json.dumps(row) + "\n" for row in rows))
        with self.assertRaisesRegex(evidence.EvidenceError, "load error"):
            self.fixture.collect(
                "load-error", [self.fixture.target("linux-amd64", report)]
            )

        outside = self.fixture.root / "outside"
        outside.mkdir()
        (outside / "LICENSE").write_bytes(MIT)
        escaped = self.fixture.report(
            "escaped", [("example.org/escape", "v1.0.0", outside)]
        )
        with self.assertRaisesRegex(evidence.EvidenceError, "escapes"):
            self.fixture.collect(
                "escape", [self.fixture.target("linux-amd64", escaped)]
            )

    def test_duplicate_or_wrong_target_is_rejected_before_output(self) -> None:
        report = self.fixture.report("amd64", [])
        target = self.fixture.target("linux-amd64", report)
        with self.assertRaisesRegex(evidence.EvidenceError, "boundary is invalid"):
            self.fixture.collect("duplicate", [target, target])
        wrong = evidence.Target("darwin-arm64", "darwin", "arm64", report)
        with self.assertRaisesRegex(evidence.EvidenceError, "malformed"):
            self.fixture.collect("wrong", [wrong])

    def test_binary_architecture_must_match_target(self) -> None:
        report = self.fixture.report("arm64", [])
        with self.assertRaisesRegex(evidence.EvidenceError, "architecture differ"):
            self.fixture.collect(
                "wrong-binary", [self.fixture.target("linux-arm64", report)]
            )

    def test_ca_bundle_must_match_exact_owning_package_and_bytes(self) -> None:
        report = self.fixture.report("amd64", [])
        self.fixture.ca_bundle.write_bytes(b"replacement")
        with self.assertRaisesRegex(evidence.EvidenceError, "pinned source input"):
            self.fixture.collect(
                "wrong-ca", [self.fixture.target("linux-amd64", report)]
            )

        self.fixture.ca_bundle.write_bytes(b"fixture CA bundle")
        self.fixture.apk_installed.write_text(
            self.fixture.apk_installed.read_text().replace(
                "o:ca-certificates", "o:different-origin"
            )
        )
        with self.assertRaisesRegex(evidence.EvidenceError, "provenance differs"):
            self.fixture.collect(
                "wrong-owner", [self.fixture.target("linux-amd64", report)]
            )


if __name__ == "__main__":
    unittest.main()
