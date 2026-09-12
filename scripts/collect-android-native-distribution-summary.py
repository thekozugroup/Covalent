#!/usr/bin/env python3
"""Bind packaged Android native objects to link and notice evidence.

This collector classifies evidence already produced by the native builders. It
does not decide license obligations, approve a release, or infer static inputs
from DT_NEEDED entries.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import stat
import sys
import zipfile
from collections import Counter
from typing import Any, Iterable


ABIS = ("arm64-v8a", "x86_64")
COMPONENTS = ("guardian", "jni", "syncthing")
LIBRARIES = {
    "guardian": "libengineguardian.so",
    "jni": "libcovalent_android_jni.so",
    "syncthing": "libsyncthing.so",
}
EXPECTED_NDK = "27.1.12297006"
EXPECTED_RECORDS = {(component, abi) for component in COMPONENTS for abi in ABIS}
EXPECTED_NOTICE_NAMES = {"NOTICE", "NOTICE.toolchain"}
ALLOWED_CATEGORIES = {
    "android-crt",
    "android-platform-stub",
    "compiler-rt-builtins",
    "llvm-cxx-runtime",
    "llvm-unwind",
}
HEX_SHA256 = re.compile(r"[0-9a-f]{64}")
MAX_RECORD_BYTES = 8 * 1024 * 1024
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_NATIVE_BYTES = 64 * 1024 * 1024
MAX_NOTICE_FILE_BYTES = 64 * 1024 * 1024
MAX_NOTICE_TOTAL_BYTES = 64 * 1024 * 1024
MAX_PACKAGE_BYTES = 256 * 1024 * 1024
MAX_PACKAGE_ENTRIES = 100_000
MAX_LINK_INPUTS = 4_096
MAX_DYNAMIC_LIBRARIES = 64
MAX_SUMMARY_BYTES = 1024 * 1024


class SummaryError(Exception):
    """A fixed, non-path-bearing evidence failure."""


def _pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise SummaryError("JSON contains a duplicate field")
        value[key] = item
    return value


def _decode_json(raw: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(raw, object_pairs_hook=_pairs)
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise SummaryError(f"{label} is malformed") from error
    if not isinstance(value, dict):
        raise SummaryError(f"{label} is malformed")
    return value


def _read_regular(path: pathlib.Path, maximum: int) -> bytes:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise SummaryError("required evidence is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= maximum:
            raise SummaryError("required evidence is not a bounded regular file")
        retained = bytearray()
        while len(retained) <= maximum:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(retained)))
            if not chunk:
                break
            retained.extend(chunk)
        after = os.fstat(descriptor)
        identity = lambda value: (
            value.st_dev,
            value.st_ino,
            value.st_size,
            value.st_mtime_ns,
            value.st_ctime_ns,
        )
        if len(retained) != before.st_size or len(retained) > maximum or identity(before) != identity(after):
            raise SummaryError("required evidence changed during collection")
        return bytes(retained)
    finally:
        os.close(descriptor)


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _bounded_digest(value: Any, label: str) -> tuple[int, str]:
    if not isinstance(value, dict):
        raise SummaryError(f"{label} descriptor is malformed")
    size, digest = value.get("bytes"), value.get("sha256")
    if (
        not isinstance(size, int)
        or isinstance(size, bool)
        or not 0 < size <= MAX_NATIVE_BYTES
        or not isinstance(digest, str)
        or HEX_SHA256.fullmatch(digest) is None
    ):
        raise SummaryError(f"{label} descriptor is malformed")
    return size, digest


def _record_summary(raw: bytes) -> tuple[tuple[str, str], dict[str, Any], dict[str, tuple[int, str]]]:
    value = _decode_json(raw, "native provenance record")
    component, abi = value.get("component"), value.get("abi")
    if not isinstance(component, str) or not isinstance(abi, str):
        raise SummaryError("native provenance record identity is unsupported")
    key = (component, abi)
    if (
        value.get("schemaVersion") != 1
        or value.get("status") != "link-inputs-classified-review-required"
        or key not in EXPECTED_RECORDS
    ):
        raise SummaryError("native provenance record identity is unsupported")
    ndk = value.get("ndk")
    if not isinstance(ndk, dict) or ndk.get("revision") != EXPECTED_NDK:
        raise SummaryError("native provenance NDK binding differs")
    files = ndk.get("files")
    if not isinstance(files, dict) or set(files) != {"source.properties", *EXPECTED_NOTICE_NAMES}:
        raise SummaryError("native provenance NDK file set is incomplete")
    ndk_files = {name: _bounded_digest(record, "NDK file") for name, record in files.items()}
    evidence = value.get("evidence")
    if not isinstance(evidence, dict):
        raise SummaryError("native binary descriptor is malformed")
    binary = _bounded_digest(evidence.get("binary"), "native binary")
    dynamics = value.get("dynamicLibraries")
    if (
        not isinstance(dynamics, list)
        or len(dynamics) > MAX_DYNAMIC_LIBRARIES
        or any(
            not isinstance(item, str)
            or not item.isascii()
            or len(item) > 128
            for item in dynamics
        )
        or dynamics != sorted(set(dynamics))
    ):
        raise SummaryError("native dynamic-library evidence is malformed")

    category_counts: dict[str, dict[str, int]] = {}
    for field in ("requestedNdkInputs", "contributingNdkInputs"):
        rows = value.get(field)
        if not isinstance(rows, list) or not rows or len(rows) > MAX_LINK_INPUTS:
            raise SummaryError("native link-input evidence is malformed")
        kinds: list[str] = []
        for row in rows:
            kind = row.get("kind") if isinstance(row, dict) else None
            if not isinstance(kind, str) or kind not in ALLOWED_CATEGORIES:
                raise SummaryError("native link-input category is unsupported")
            kinds.append(kind)
        category_counts[field] = dict(sorted(Counter(kinds).items()))
    summary = {
        "abi": abi,
        "component": component,
        "binary": {"bytes": binary[0], "sha256": binary[1]},
        "contributingCategories": category_counts["contributingNdkInputs"],
        "dynamicLibraries": dynamics,
        "record": {"bytes": len(raw), "sha256": _digest(raw)},
        "requestedCategories": category_counts["requestedNdkInputs"],
    }
    return key, summary, ndk_files


def _zip_index(archive: zipfile.ZipFile) -> dict[str, zipfile.ZipInfo]:
    infos = archive.infolist()
    if len(infos) > MAX_PACKAGE_ENTRIES:
        raise SummaryError("Android package entry bound exceeded")
    result: dict[str, zipfile.ZipInfo] = {}
    for info in infos:
        if info.filename in result:
            raise SummaryError("Android package contains duplicate entries")
        result[info.filename] = info
    return result


def _read_zip_entry(
    archive: zipfile.ZipFile,
    index: dict[str, zipfile.ZipInfo],
    name: str,
    maximum: int,
) -> bytes:
    info = index.get(name)
    if info is None or info.is_dir() or not 0 < info.file_size <= maximum:
        raise SummaryError("required Android package entry is missing or exceeds its bound")
    retained = bytearray()
    try:
        with archive.open(info) as source:
            while len(retained) <= maximum:
                chunk = source.read(min(1024 * 1024, maximum + 1 - len(retained)))
                if not chunk:
                    break
                retained.extend(chunk)
    except (OSError, RuntimeError, zipfile.BadZipFile) as error:
        raise SummaryError("Android package entry is unreadable") from error
    if len(retained) != info.file_size or len(retained) > maximum:
        raise SummaryError("Android package entry changed or exceeds its bound")
    return bytes(retained)


def _safe_bundle_path(value: Any) -> str:
    try:
        encoded_length = len(value.encode("utf-8")) if isinstance(value, str) else 0
    except UnicodeEncodeError as error:
        raise SummaryError("notice bundle path is malformed") from error
    if (
        not isinstance(value, str)
        or not value
        or encoded_length > 4096
        or "\\" in value
        or any(ord(character) < 32 or ord(character) == 127 for character in value)
    ):
        raise SummaryError("notice bundle path is malformed")
    path = pathlib.PurePosixPath(value)
    if path.is_absolute() or "." in path.parts or ".." in path.parts:
        raise SummaryError("notice bundle path is malformed")
    return path.as_posix()


def _native_manifest(raw: bytes) -> dict[str, tuple[int, str]]:
    try:
        text = raw.decode("ascii")
    except UnicodeDecodeError as error:
        raise SummaryError("native binary manifest is malformed") from error
    if not text.endswith("\n") or "\r" in text:
        raise SummaryError("native binary manifest is malformed")
    rows: dict[str, tuple[int, str]] = {}
    for line in text[:-1].split("\n"):
        fields = line.split(" ")
        if len(fields) != 3:
            raise SummaryError("native binary manifest is malformed")
        abi, digest, size_text = fields
        if (
            abi not in ABIS
            or abi in rows
            or HEX_SHA256.fullmatch(digest) is None
            or not size_text.isascii()
            or not size_text.isdecimal()
            or len(size_text) > 20
        ):
            raise SummaryError("native binary manifest is malformed")
        size = int(size_text)
        if str(size) != size_text or not 0 < size <= MAX_NATIVE_BYTES:
            raise SummaryError("native binary manifest is malformed")
        rows[abi] = (size, digest)
    if set(rows) != set(ABIS):
        raise SummaryError("native binary manifest is incomplete")
    return rows


def _declared_notice_files(manifest: dict[str, Any]) -> dict[str, tuple[int | None, str]]:
    declared: dict[str, tuple[int | None, str]] = {}

    def retain(path_value: Any, size: Any, digest: Any) -> None:
        path = _safe_bundle_path(path_value)
        if not isinstance(digest, str) or HEX_SHA256.fullmatch(digest) is None:
            raise SummaryError("notice file descriptor is malformed or duplicated")
        if size is not None and (
            not isinstance(size, int)
            or isinstance(size, bool)
            or not 0 < size <= MAX_NOTICE_FILE_BYTES
        ):
            raise SummaryError("notice file descriptor is malformed or duplicated")
        descriptor = (size, digest)
        if path in declared and declared[path] != descriptor:
            raise SummaryError("notice file descriptor is malformed or duplicated")
        declared[path] = descriptor

    pending: list[Any] = [manifest]
    visited = 0
    while pending:
        value = pending.pop()
        visited += 1
        if visited > 100_000:
            raise SummaryError("notice manifest collection bound exceeded")
        if isinstance(value, list):
            pending.extend(value)
        elif isinstance(value, dict):
            if "bundlePath" in value:
                retain(value["bundlePath"], value.get("bytes"), value.get("sha256"))
            if "sourceBundlePath" in value:
                retain(value["sourceBundlePath"], value.get("sourceBytes"), value.get("sourceSha256"))
            if "licenseBundlePath" in value:
                retain(value["licenseBundlePath"], value.get("licenseBytes"), value.get("licenseSha256"))
            pending.extend(value.values())
    if "THIRD-PARTY-NOTICES.txt" not in declared:
        raise SummaryError("notice manifest omits the readable combined notice")
    return declared


def _package_summary(
    path: pathlib.Path,
    records: dict[tuple[str, str], dict[str, Any]],
) -> tuple[dict[str, Any], bytes, dict[str, tuple[int, str]]]:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        package_fd = os.open(path, flags)
    except OSError as error:
        raise SummaryError("Android package is unavailable") from error
    try:
        metadata = os.fstat(package_fd)
        if not stat.S_ISREG(metadata.st_mode) or not 0 < metadata.st_size <= MAX_PACKAGE_BYTES:
            raise SummaryError("Android package is missing or exceeds its bound")
        package_digest = hashlib.sha256()
        retained = 0
        while retained <= MAX_PACKAGE_BYTES:
            chunk = os.read(package_fd, min(1024 * 1024, MAX_PACKAGE_BYTES + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            package_digest.update(chunk)
        if retained != metadata.st_size or retained > MAX_PACKAGE_BYTES:
            raise SummaryError("Android package changed or exceeds its bound")
        os.lseek(package_fd, 0, os.SEEK_SET)
        with os.fdopen(os.dup(package_fd), "rb") as package_file, zipfile.ZipFile(package_file) as archive:
            index = _zip_index(archive)
            binary_manifests = {
                component: _native_manifest(
                    _read_zip_entry(archive, index, f"assets/{asset}", 1024)
                )
                for component, asset in (
                    ("guardian", "engine-guardian-sha256.txt"),
                    ("syncthing", "syncthing-sha256.txt"),
                )
            }
            native: list[dict[str, Any]] = []
            for component in COMPONENTS:
                for abi in ABIS:
                    entry = f"lib/{abi}/{LIBRARIES[component]}"
                    data = _read_zip_entry(archive, index, entry, MAX_NATIVE_BYTES)
                    observed = (len(data), _digest(data))
                    expected = records[(component, abi)]["binary"]
                    if observed != (expected["bytes"], expected["sha256"]):
                        raise SummaryError("packaged native object differs from link evidence")
                    if component in binary_manifests and observed != binary_manifests[component][abi]:
                        raise SummaryError("packaged native object differs from its manifest")
                    native.append({
                        "abi": abi,
                        "bytes": observed[0],
                        "component": component,
                        "sha256": observed[1],
                    })
            manifest_raw = _read_zip_entry(
                archive, index, "assets/sync-engine-notices/manifest.json", MAX_MANIFEST_BYTES
            )
            manifest = _decode_json(manifest_raw, "notice manifest")
            if manifest.get("schemaVersion") != 1 or manifest.get("status") != "texts-collected-review-required":
                raise SummaryError("notice manifest schema is unsupported")
            declared = _declared_notice_files(manifest)
            total = 0
            verified: dict[str, tuple[int, str]] = {}
            for relative, (expected_size, expected_digest) in sorted(declared.items()):
                data = _read_zip_entry(
                    archive,
                    index,
                    "assets/sync-engine-notices/" + relative,
                    MAX_NOTICE_FILE_BYTES,
                )
                total += len(data)
                if total > MAX_NOTICE_TOTAL_BYTES:
                    raise SummaryError("packaged notice aggregate bound exceeded")
                if (expected_size is not None and len(data) != expected_size) or _digest(data) != expected_digest:
                    raise SummaryError("packaged notice file differs from its manifest")
                verified[relative] = (len(data), _digest(data))
            index_raw = _read_zip_entry(
                archive, index, "assets/sync-engine-notices-index.txt", 1024
            )
            try:
                index_text = index_raw.decode("ascii")
            except UnicodeDecodeError as error:
                raise SummaryError("packaged notice index is malformed") from error
            index_lines = index_text.splitlines()
            if not index_text.endswith("\n") or "\r" in index_text or len(index_lines) != 3 or index_lines[0] != "1":
                raise SummaryError("packaged notice index is malformed")
            indexed: dict[str, tuple[int, str, str]] = {}
            for line in index_lines[1:]:
                fields = line.split(" ")
                if len(fields) != 4 or fields[0] in indexed:
                    raise SummaryError("packaged notice index is malformed")
                label, digest, size_text, relative = fields
                if (
                    label not in {"combined", "manifest"}
                    or HEX_SHA256.fullmatch(digest) is None
                    or not size_text.isascii()
                    or not size_text.isdecimal()
                    or len(size_text) > 20
                ):
                    raise SummaryError("packaged notice index is malformed")
                size = int(size_text)
                if str(size) != size_text or not 0 < size <= MAX_NOTICE_FILE_BYTES:
                    raise SummaryError("packaged notice index is malformed")
                indexed[label] = (size, digest, relative)
            combined = verified["THIRD-PARTY-NOTICES.txt"]
            expected_index = {
                "combined": (combined[0], combined[1], "sync-engine-notices/THIRD-PARTY-NOTICES.txt"),
                "manifest": (len(manifest_raw), _digest(manifest_raw), "sync-engine-notices/manifest.json"),
            }
            if indexed != expected_index:
                raise SummaryError("packaged notice index differs from its manifest")
            rows = manifest.get("toolchainNotices")
            if not isinstance(rows, list) or len(rows) != 2:
                raise SummaryError("notice manifest lacks the exact NDK notice pair")
            ndk_notices: dict[str, tuple[int, str]] = {}
            for row in rows:
                name = row.get("sourceName") if isinstance(row, dict) else None
                if not isinstance(name, str) or name not in EXPECTED_NOTICE_NAMES:
                    raise SummaryError("notice manifest NDK descriptor is malformed")
                if name in ndk_notices or row.get("bundlePath") != f"toolchain/{name}":
                    raise SummaryError("notice manifest NDK descriptor is malformed")
                descriptor = _bounded_digest(row, "NDK notice")
                if verified.get(f"toolchain/{name}") != descriptor:
                    raise SummaryError("notice manifest NDK descriptor is incomplete")
                ndk_notices[name] = descriptor
            if set(ndk_notices) != EXPECTED_NOTICE_NAMES:
                raise SummaryError("notice manifest lacks the exact NDK notice pair")
        after = os.fstat(package_fd)
        identity = lambda value: (
            value.st_dev,
            value.st_ino,
            value.st_size,
            value.st_mtime_ns,
            value.st_ctime_ns,
        )
        if identity(metadata) != identity(after):
            raise SummaryError("Android package changed during collection")
    except (OSError, zipfile.BadZipFile) as error:
        raise SummaryError("Android package is unreadable") from error
    finally:
        os.close(package_fd)
    return (
        {
            "bytes": metadata.st_size,
            "nativeObjects": native,
            "notices": {
                "combined": {"bytes": combined[0], "sha256": combined[1]},
                "files": len(verified),
                "manifest": {"bytes": len(manifest_raw), "sha256": _digest(manifest_raw)},
                "totalBytes": total,
            },
            "sha256": package_digest.hexdigest(),
        },
        manifest_raw,
        ndk_notices,
    )


def collect(record_paths: list[pathlib.Path], package_paths: list[pathlib.Path]) -> dict[str, Any]:
    if len(record_paths) != 6 or len(package_paths) != 2:
        raise SummaryError("exactly six provenance records and two Android packages are required")
    records: dict[tuple[str, str], dict[str, Any]] = {}
    ndk_files: dict[str, tuple[int, str]] | None = None
    for path in record_paths:
        raw = _read_regular(path, MAX_RECORD_BYTES)
        key, summary, observed_ndk = _record_summary(raw)
        if key in records:
            raise SummaryError("native provenance record target and kind are duplicated")
        if ndk_files is not None and observed_ndk != ndk_files:
            raise SummaryError("native provenance records use different NDK notice bytes")
        ndk_files = observed_ndk
        records[key] = summary
    if set(records) != EXPECTED_RECORDS or ndk_files is None:
        raise SummaryError("native provenance record set is incomplete")

    packages: list[dict[str, Any]] = []
    notice_manifest: bytes | None = None
    for variant, path in zip(("debug", "release"), package_paths, strict=True):
        package, observed_manifest, observed_notices = _package_summary(path, records)
        package["variant"] = variant
        if notice_manifest is not None and observed_manifest != notice_manifest:
            raise SummaryError("Android packages contain different notice manifests")
        if observed_notices != {name: ndk_files[name] for name in EXPECTED_NOTICE_NAMES}:
            raise SummaryError("packaged NDK notices differ from native link evidence")
        notice_manifest = observed_manifest
        packages.append(package)
    if packages[0]["nativeObjects"] != packages[1]["nativeObjects"]:
        raise SummaryError("Android packages contain different native objects")

    requested = Counter()
    contributing = Counter()
    dynamics = Counter()
    ordered_records = [records[key] for key in sorted(records)]
    for record in ordered_records:
        requested.update(record["requestedCategories"])
        contributing.update(record["contributingCategories"])
        dynamics.update(record["dynamicLibraries"])
    return {
        "schemaVersion": 1,
        "status": "packaged-native-evidence-bound-review-required",
        "scope": (
            "Exact debug/release native object bytes, six final-link records, and packaged notice "
            "digests; classification evidence only, with no license or release approval."
        ),
        "ndk": {
            "files": {
                name: {"bytes": ndk_files[name][0], "sha256": ndk_files[name][1]}
                for name in sorted(ndk_files)
            },
            "revision": EXPECTED_NDK,
        },
        "packages": packages,
        "records": ordered_records,
        "totals": {
            "contributingCategories": dict(sorted(contributing.items())),
            "dynamicLibraries": dict(sorted(dynamics.items())),
            "requestedCategories": dict(sorted(requested.items())),
        },
    }


def _encoded(value: dict[str, Any]) -> bytes:
    raw = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()
    if len(raw) > MAX_SUMMARY_BYTES:
        raise SummaryError("native distribution summary exceeds its bound")
    return raw


def _write_new(path: pathlib.Path, data: bytes) -> None:
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0), 0o600)
    except OSError as error:
        raise SummaryError("summary output is unavailable") from error
    try:
        written = 0
        while written < len(data):
            count = os.write(descriptor, data[written:])
            if count <= 0:
                raise SummaryError("summary output could not be completed")
            written += count
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _print_receipt(value: dict[str, Any], encoded: bytes) -> None:
    compact = lambda item: json.dumps(item, sort_keys=True, separators=(",", ":"))
    for record in value["records"]:
        print("ANDROID_NATIVE_RECORD " + compact(record))
    print("ANDROID_NDK_EVIDENCE " + compact(value["ndk"]))
    packages = [
        {
            "bytes": package["bytes"],
            "notices": package["notices"],
            "sha256": package["sha256"],
            "variant": package["variant"],
        }
        for package in value["packages"]
    ]
    print("ANDROID_NATIVE_SUMMARY " + compact({
        "packages": packages,
        "records": len(value["records"]),
        "sha256": _digest(encoded),
        "status": value["status"],
        "summaryBytes": len(encoded),
        "totals": value["totals"],
    }))


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--record", action="append", required=True, type=pathlib.Path)
    parser.add_argument("--debug-package", required=True, type=pathlib.Path)
    parser.add_argument("--release-package", required=True, type=pathlib.Path)
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--output", type=pathlib.Path)
    destination.add_argument("--verify-output", type=pathlib.Path)
    arguments = parser.parse_args(argv)
    try:
        value = collect(arguments.record, [arguments.debug_package, arguments.release_package])
        encoded = _encoded(value)
        if arguments.output is not None:
            _write_new(arguments.output, encoded)
        else:
            if _read_regular(arguments.verify_output, MAX_SUMMARY_BYTES) != encoded:
                raise SummaryError("stored native distribution summary differs")
    except SummaryError as error:
        print(f"Android native distribution evidence failed: {error}", file=sys.stderr)
        return 1
    _print_receipt(value, encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
