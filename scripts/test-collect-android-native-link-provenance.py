#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("collect-android-native-link-provenance.py")
SPEC = importlib.util.spec_from_file_location("android_link_provenance", SCRIPT)
assert SPEC and SPEC.loader
provenance = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = provenance
SPEC.loader.exec_module(provenance)


class Fixture:
    def __init__(self, component: str = "syncthing") -> None:
        self.temp = tempfile.TemporaryDirectory(dir="/tmp", prefix="cv-link-proof-")
        self.root = pathlib.Path(self.temp.name).resolve(strict=True)
        self.ndk = self.root / "ndk"
        self.evidence = self.root / "evidence"
        self.ndk.mkdir()
        self.evidence.mkdir()
        (self.ndk / "source.properties").write_text("Pkg.Revision = 27.1.12297006\n")
        (self.ndk / "NOTICE").write_text("NDK notice\n")
        (self.ndk / "NOTICE.toolchain").write_text("LLVM notice\n")
        self.component = component
        self.map = self.evidence / "link.map"
        self.trace = self.evidence / "driver.txt"
        self.dynamic = self.evidence / "dynamic.txt"
        self.binary = self.evidence / "binary.so"
        self.binary.write_bytes(b"\x7fELFfixture")
        if component == "jni":
            begin, end = "crtbegin_so.o", "crtend_so.o"
        else:
            begin, end = "crtbegin_dynamic.o", "crtend_android.o"
        sysroot = self.ndk / "toolchains/llvm/prebuilt/linux/sysroot/usr/lib/aarch64-linux-android/26"
        runtime = self.ndk / "toolchains/llvm/prebuilt/linux/lib/clang/18/lib/linux"
        linker = self.ndk / "toolchains/llvm/prebuilt/linux/bin/ld.lld"
        self.begin = sysroot / begin
        self.end = sysroot / end
        self.builtins = runtime / "libclang_rt.builtins-aarch64-android.a"
        for path in (self.begin, self.end, self.builtins, linker):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fixture")
        self.trace.write_text(
            "Android clang version fixture\n"
            + " ".join(
                f'"{value}"'
                for value in (
                    linker,
                    "-pie" if component != "jni" else "-shared",
                    self.begin,
                    self.builtins,
                    "-lc",
                    self.end,
                )
            )
            + "\n"
        )
        self.map.write_text(
            "             VMA              LMA     Size Align Out     In      Symbol\n"
            f"          0x1000           0x1000       20     4 .text   {self.begin}:(.text)\n"
            f"          0x1020           0x1020       10     4         {self.builtins}(mulodi4.c.o):(.text.__mulodi4)\n"
        )
        libs = "libc.so" if component == "guardian" else "libc.so libdl.so liblog.so"
        self.dynamic.write_text(
            "\n".join(f"  0x0000000000000001 (NEEDED) Shared library: [{name}]" for name in libs.split()) + "\n"
        )

    def collect(self):
        return provenance.collect(
            self.component,
            "arm64-v8a",
            self.ndk,
            "27.1.12297006",
            self.map,
            self.trace,
            self.dynamic,
            self.binary,
        )

    def close(self) -> None:
        self.temp.cleanup()


class LinkProvenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_realistic_pie_map_classifies_requested_and_extracted_inputs(self) -> None:
        value = self.fixture.collect()
        self.assertEqual(value["component"], "syncthing")
        self.assertEqual(value["evidence"]["binary"]["bytes"], len(self.fixture.binary.read_bytes()))
        self.assertEqual(value["dynamicLibraries"], ["libc.so", "libdl.so", "liblog.so"])
        self.assertEqual(
            {row["kind"] for row in value["requestedNdkInputs"]},
            {"android-crt", "compiler-rt-builtins"},
        )
        extracted = [
            row
            for row in value["contributingNdkInputs"]
            if row["kind"] == "compiler-rt-builtins"
        ]
        self.assertEqual(extracted[0]["archiveMember"], "mulodi4.c.o")
        encoded = json.dumps(value)
        self.assertNotIn(str(self.fixture.root), encoded)
        self.assertIn("${NDK}/", encoded)

    def test_shared_object_requires_shared_crt_and_classifies_optional_runtimes(self) -> None:
        self.fixture.close()
        self.fixture = Fixture("jni")
        runtime = self.fixture.ndk / "toolchains/llvm/prebuilt/linux/sysroot/usr/lib/aarch64-linux-android"
        for name in ("libunwind.a", "libc++_static.a"):
            (runtime / name).parent.mkdir(parents=True, exist_ok=True)
            (runtime / name).write_bytes(b"fixture")
        trace = self.fixture.trace.read_text()
        trace = trace.replace(
            f'"{self.fixture.end}"',
            f'"-l:libunwind.a" "-lc++_static" "{self.fixture.end}"',
        )
        self.fixture.trace.write_text(trace + "ignored compiler diagnostic\n")
        self.fixture.map.write_text(
            self.fixture.map.read_text()
            + f"  0x1030 0x1030 10 4 {runtime / 'libunwind.a'}(UnwindLevel1.c.o):(.text)\n"
            + f"  0x1040 0x1040 10 4 {runtime / 'libc++_static.a'}(string.cpp.o):(.text)\n"
        )
        value = self.fixture.collect()
        self.assertEqual(
            {row["kind"] for row in value["contributingNdkInputs"]},
            {"android-crt", "compiler-rt-builtins", "llvm-cxx-runtime", "llvm-unwind"},
        )
        self.assertEqual(
            {row["argument"] for row in value["requestedLinkLibraries"]},
            {"-l:libunwind.a", "-lc", "-lc++_static"},
        )

    def test_named_library_request_matches_only_the_exact_resolved_basename(self) -> None:
        runtime = self.fixture.ndk / "toolchains/llvm/prebuilt/linux/lib/clang/18/lib/linux/aarch64"
        unwind = runtime / "libunwind.a"
        unwind.parent.mkdir(parents=True, exist_ok=True)
        unwind.write_bytes(b"fixture")
        self.fixture.trace.write_text(
            self.fixture.trace.read_text().replace('"-lc"', '"-lunwind" "-lc"')
        )
        self.fixture.map.write_text(
            self.fixture.map.read_text()
            + f"  0x1030 0x1030 10 4 {unwind}(UnwindLevel1.c.o):(.text)\n"
        )
        value = self.fixture.collect()
        self.assertIn(
            {"argument": "-lunwind", "candidateFiles": ["libunwind.a", "libunwind.so"]},
            value["requestedLinkLibraries"],
        )

        self.fixture.trace.write_text(
            self.fixture.trace.read_text().replace('"-lunwind"', '"-l:libc++_static.a"')
        )
        with self.assertRaisesRegex(
            provenance.ProvenanceError,
            r"absent from the driver trace: llvm-unwind:\$\{NDK\}/",
        ):
            self.fixture.collect()

    def test_wrong_crt_unknown_ndk_input_and_missing_builtins_fail_closed(self) -> None:
        self.fixture.end.with_name("crtend_so.o").write_bytes(b"fixture")
        self.fixture.trace.write_text(self.fixture.trace.read_text().replace("crtend_android.o", "crtend_so.o"))
        with self.assertRaisesRegex(provenance.ProvenanceError, "expected CRT pair"):
            self.fixture.collect()

        self.fixture.close()
        self.fixture = Fixture()
        unknown = self.fixture.ndk / "toolchains/llvm/prebuilt/linux/lib/unknown.a"
        unknown.write_bytes(b"unknown")
        self.fixture.map.write_text(self.fixture.map.read_text() + f"  0 0 1 1 {unknown}(x.o):(.text)\n")
        with self.assertRaisesRegex(provenance.ProvenanceError, "unclassified NDK input"):
            self.fixture.collect()

        self.fixture.close()
        self.fixture = Fixture()
        self.fixture.trace.write_text(self.fixture.trace.read_text().replace(str(self.fixture.builtins), "-lmissing"))
        with self.assertRaisesRegex(provenance.ProvenanceError, "compiler-rt builtins"):
            self.fixture.collect()

    def test_bad_revision_symlink_notice_and_evidence_bounds_fail_closed(self) -> None:
        with self.assertRaisesRegex(provenance.ProvenanceError, "revision differs"):
            provenance.collect(
                "syncthing", "arm64-v8a", self.fixture.ndk, "27.1.999", self.fixture.map,
                self.fixture.trace, self.fixture.dynamic, self.fixture.binary,
            )
        notice = self.fixture.ndk / "NOTICE"
        target = self.fixture.root / "outside-notice"
        target.write_text("outside\n")
        notice.unlink()
        notice.symlink_to(target)
        with self.assertRaisesRegex(provenance.ProvenanceError, "unavailable"):
            self.fixture.collect()

        notice.unlink()
        notice.write_text("NDK notice\n")
        with mock.patch.object(provenance, "MAX_MAP_BYTES", 8):
            with self.assertRaisesRegex(provenance.ProvenanceError, "byte bound"):
                self.fixture.collect()

    def test_in_root_dot_segments_resolve_and_escape_is_rejected(self) -> None:
        (self.fixture.builtins.parent / "nested").mkdir()
        dotted = self.fixture.builtins.parent / "nested" / ".." / self.fixture.builtins.name
        self.fixture.trace.write_text(self.fixture.trace.read_text().replace(str(self.fixture.builtins), str(dotted)))
        self.fixture.map.write_text(self.fixture.map.read_text().replace(str(self.fixture.builtins), str(dotted)))
        self.fixture.collect()

        outside = self.fixture.root / "outside.a"
        outside.write_bytes(b"outside")
        escaped = self.fixture.ndk / "toolchains" / ".." / ".." / outside.name
        self.fixture.trace.write_text(self.fixture.trace.read_text().replace(str(dotted), str(escaped)))
        with self.assertRaisesRegex(provenance.ProvenanceError, "out-of-root"):
            self.fixture.collect()

        self.fixture.close()
        self.fixture = Fixture()
        outside = self.fixture.root / "outside-builtins.a"
        outside.write_bytes(b"outside")
        self.fixture.builtins.unlink()
        self.fixture.builtins.symlink_to(outside)
        with self.assertRaisesRegex(provenance.ProvenanceError, "out-of-root"):
            self.fixture.collect()

    def test_go_link_classifier_handles_probes_final_response_and_ambiguity(self) -> None:
        private = self.fixture.root / "private"
        private.mkdir()
        go_object = private / "go.o"
        go_object.write_bytes(b"object")
        expected = private / "linked-output"
        probe = private / "trivial.c"
        probe.write_text("int main(void) { return 0; }")
        self.assertEqual(
            "probe",
            provenance.classify_go_link_invocation(
                ["-o", str(private / "a.out"), "-Wl,--build-id", str(probe)], private
            ),
        )
        self.assertEqual(
            "probe", provenance.classify_go_link_invocation(["--version"], private)
        )
        empty_response = private / "empty-response"
        empty_response.write_bytes(b"")
        self.assertEqual(
            "probe",
            provenance.classify_go_link_invocation(
                ["-o", str(private / "a.out"), "@" + str(empty_response), str(probe)],
                private,
            ),
        )
        response = private / "response"
        response.write_text(
            "\n".join(json.dumps(value) for value in ("-o", str(expected), str(go_object))) + "\n"
        )
        self.assertEqual(
            "final", provenance.classify_go_link_invocation(["@" + str(response)], private)
        )
        response.write_text(json.dumps("@nested") + "\n")
        with self.assertRaisesRegex(provenance.ProvenanceError, "malformed"):
            provenance.classify_go_link_invocation(["@" + str(response)], private)

    def test_external_link_wrapper_passes_probes_and_records_only_one_final_link(self) -> None:
        private = self.fixture.root / "private"
        private.mkdir()
        expected = private / "linked-output"
        go_object = private / "go.o"
        go_object.write_bytes(b"object")
        response = private / "response"
        response.write_text(
            "\n".join(json.dumps(value) for value in ("-o", str(expected), str(go_object))) + "\n"
        )
        compiler = private / "clang"
        compiler.write_text(
            "#!/bin/sh\nset -eu\n"
            "for value in \"$@\"; do\n"
            "  if test \"$value\" = -###; then echo '\"/ndk/bin/ld.lld\" fixture' >&2; exit 0; fi\n"
            "done\n"
            "output= map= previous=\n"
            "for value in \"$@\"; do\n"
            "  if test \"$previous\" = -o; then output=$value; fi\n"
            "  case \"$value\" in -Wl,-Map,*) map=${value#-Wl,-Map,} ;; esac\n"
            "  previous=$value\n"
            "done\n"
            "test -n \"$output\" && : > \"$output\"\n"
            "test -z \"$map\" || printf 'fixture map\\n' > \"$map\"\n"
        )
        compiler.chmod(0o755)
        marker = private / "final.marker"
        link_map = private / "final.map"
        trace = private / "driver.txt"
        environment = {
            "PATH": os.environ["PATH"],
            "COVALENT_REAL_CLANG": str(compiler),
            "COVALENT_LINK_CLASSIFIER": str(SCRIPT),
            "COVALENT_LINK_PRIVATE_ROOT": str(private),
            "COVALENT_LINK_MAP": str(link_map),
            "COVALENT_DRIVER_TRACE": str(trace),
            "COVALENT_LINK_MARKER": str(marker),
        }
        wrapper = SCRIPT.with_name("android-go-link-wrapper.sh")
        probe_output = private / "a.out"
        subprocess.run(
            [str(wrapper), "-o", str(probe_output), str(private / "trivial.c")],
            env=environment,
            check=True,
        )
        self.assertTrue(probe_output.is_file())
        self.assertFalse(marker.exists())
        subprocess.run([str(wrapper), "@" + str(response)], env=environment, check=True)
        self.assertTrue(marker.is_dir())
        self.assertEqual("fixture map\n", link_map.read_text())
        self.assertIn("ld.lld", trace.read_text())
        repeated = subprocess.run(
            [str(wrapper), "@" + str(response)], env=environment, capture_output=True, text=True
        )
        self.assertNotEqual(0, repeated.returncode)
        self.assertIn("more than once", repeated.stderr)
        response.write_text(
            "\n".join(
                json.dumps(value)
                for value in ("-o", str(expected), "-o", str(private / "other"), str(go_object))
            )
            + "\n"
        )
        with self.assertRaisesRegex(provenance.ProvenanceError, "ambiguous output"):
            provenance.classify_go_link_invocation(["@" + str(response)], private)
        second = private / "second-response"
        second.write_bytes(b"")
        with self.assertRaisesRegex(provenance.ProvenanceError, "ambiguous"):
            provenance.classify_go_link_invocation(
                ["@" + str(response), "@" + str(second)], private
            )

    def test_truncated_map_unexpected_dynamic_and_duplicate_linker_trace_reject(self) -> None:
        self.fixture.map.write_text("VMA LMA Size Align Out In Symbol\n")
        with self.assertRaisesRegex(provenance.ProvenanceError, "no contributing"):
            self.fixture.collect()

        self.fixture.close()
        self.fixture = Fixture()
        self.fixture.dynamic.write_text("Shared library: [libc++_shared.so]\n")
        with self.assertRaisesRegex(provenance.ProvenanceError, "unexpected dependency"):
            self.fixture.collect()

        self.fixture.close()
        self.fixture = Fixture()
        self.fixture.trace.write_text(self.fixture.trace.read_text() * 2)
        with self.assertRaisesRegex(provenance.ProvenanceError, "exactly one linker"):
            self.fixture.collect()

    def test_unparsed_map_line_and_unrequested_archive_member_reject(self) -> None:
        self.fixture.map.write_text(
            self.fixture.map.read_text()
            + f"  0 0 1 1 {self.fixture.ndk / 'toolchains/opaque-input.bc'}\n"
        )
        with self.assertRaisesRegex(provenance.ProvenanceError, "unparsed NDK input"):
            self.fixture.collect()

        self.fixture.close()
        self.fixture = Fixture()
        other = self.fixture.builtins.with_name("libclang_rt.builtins-other-android.a")
        other.write_bytes(b"other")
        self.fixture.map.write_text(
            self.fixture.map.read_text() + f"  0 0 1 1 {other}(x.o):(.text)\n"
        )
        with self.assertRaisesRegex(provenance.ProvenanceError, "absent from the driver trace"):
            self.fixture.collect()

    def test_cli_does_not_replace_existing_output(self) -> None:
        output = self.fixture.root / "manifest.json"
        output.write_text("preserve")
        with redirect_stderr(io.StringIO()):
            status = provenance.main(
                [
                    "--component", "syncthing", "--abi", "arm64-v8a",
                    "--ndk-root", str(self.fixture.ndk), "--expected-revision", "27.1.12297006",
                    "--link-map", str(self.fixture.map), "--driver-trace", str(self.fixture.trace),
                    "--dynamic-report", str(self.fixture.dynamic), "--binary", str(self.fixture.binary),
                    "--output", str(output),
                ]
            )
        self.assertEqual(status, 1)
        self.assertEqual(output.read_text(), "preserve")


if __name__ == "__main__":
    unittest.main()
