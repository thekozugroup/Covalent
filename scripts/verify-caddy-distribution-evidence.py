#!/usr/bin/env python3
"""Verify packaged Caddy distribution evidence against the shipped binary."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import pathlib
import stat
import sys
import tarfile
import zlib
from typing import Any, Iterable


CADDY_VERSION = "v2.11.5-0.20260711231708-b2693fb63a30"
CADDY_COMMIT = "b2693fb63a30e6d7be0972c3645e9a2c0a500e93"
CONSUMER_RECORD = {
    "module": "covalent.local/caddy",
    "goModSha256": "2855429aae39f4f05afa5ac60bebb2dd0a335a4ce5620af7fec94a039e3f44f7",
    "goSumSha256": "3ac528097eb986c6c39b8a42362c0eb7753f67680f680f3a4c77f9775d6de00a",
    "mainGoSha256": "148320867ca029601e6bb4d221922f7d1f6817a6c50da2e84761e859e7505800",
    "license": "MIT",
    "licenseSha256": "ff9bc316792502655bcae384b8e5a4604da93385fda00552b77d85c334b94acd",
}
GO_TOOLCHAIN_RECORD = {
    "version": "go1.26.7",
    "files": [
        {
            "sourcePath": "LICENSE", "bytes": 1_453,
            "sha256": "911f8f5782931320f5b8d1160a76365b83aea6447ee6c04fa6d5591467db9dad",
        },
        {
            "sourcePath": "PATENTS", "bytes": 1_303,
            "sha256": "96f408bfae65bf137fc2525d3ecb030271c50c1e90799f87abf8846d8dd505cc",
        },
        {
            "sourcePath": "src/crypto/internal/boring/LICENSE", "bytes": 9_714,
            "sha256": "56210f826b8f0fbac3160dfe55c97f4019eb6cdda3963b5a0eab8a8bdb62360e",
        },
    ],
}
EXPECTED_TARGET_RECORDS = {
    "linux-amd64": {
        "packageCount": 921,
        "moduleCount": 142,
        "inventorySha256": "2a23939c5e5385730dafc7652266aa836543d10048ade190fc5835e22a30f4c8",
        "noticeSha256": "5024f49556fa614b3f835bda2557443c96172fe2c3624dd3373e8c3083b1b2ca",
        "binarySha256": "8e434b9905b36de078f40608af6d5c1df73394ef056bae6240a8563176182fac",
    },
    "linux-arm64": {
        "packageCount": 919,
        "moduleCount": 142,
        "inventorySha256": "0ce4c352f590179305fb0bef88235d3ebf6a23fb9651cb7f404c3e32e882870c",
        "noticeSha256": "5024f49556fa614b3f835bda2557443c96172fe2c3624dd3373e8c3083b1b2ca",
        "binarySha256": "80d7c728cb232c9b5914d1df3a837b3adb5481d468730088bae07a9c7438881d",
    },
}
MYSQL_SOURCE_MEMBER_MANIFEST_SHA256 = "4a565566abcab4711cb605658718e7fa8b86c5b8d436a4b11387b6d11a607110"
CA_RECORD = {
    "sourceStage": "golang:1.26.7-alpine3.23",
    "package": "ca-certificates-bundle",
    "version": "20260611-r0",
    "origin": "ca-certificates",
    "declaredLicense": "MPL-2.0 AND MIT",
    "aportsCommit": "6e30aafe4fa807ba70797509731f1e7d644dc8f3",
    "bundlePath": "/etc/ssl/certs/ca-certificates.crt",
    "bytes": 179_359,
    "sha256": "b8d837841b88bfaa1a0fa827cbca8e2576418dd47c9fc4bb7f1f9d89c83111b9",
    "reviewStatus": "metadata-bound-source-review-required",
}
MAX_JSON = 24 * 1024 * 1024
MAX_NOTICE = 16 * 1024 * 1024
MAX_BINARY = 256 * 1024 * 1024
MAX_ARCHIVE = 64 * 1024 * 1024
MAX_ARCHIVE_ENTRIES = 100_000
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_TAR_BYTES = MAX_ARCHIVE_BYTES + (MAX_ARCHIVE_ENTRIES + 32) * 1024


class VerificationError(Exception):
    """Fixed evidence validation failure."""


def _read_regular(path: pathlib.Path, maximum: int) -> bytes:
    flags = (
        os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise VerificationError("required package evidence is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
            raise VerificationError("package evidence is not a bounded regular file")
        chunks = []
        retained = 0
        while retained <= maximum:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - retained))
            if not chunk:
                break
            chunks.append(chunk)
            retained += len(chunk)
        after = os.fstat(descriptor)
        if (
            retained != before.st_size or retained > maximum
            or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
            != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        ):
            raise VerificationError("package evidence changed or exceeded its bound")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _load(path: pathlib.Path) -> tuple[dict[str, Any], bytes]:
    raw = _read_regular(path, MAX_JSON)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise VerificationError("package evidence JSON is malformed") from error
    if not isinstance(value, dict):
        raise VerificationError("package evidence JSON has the wrong shape")
    return value, raw


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _safe_relative(raw: Any) -> pathlib.PurePosixPath:
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise VerificationError("package evidence path is malformed")
    path = pathlib.PurePosixPath(raw)
    if path.is_absolute() or "." in path.parts or ".." in path.parts or len(path.parts) > 64:
        raise VerificationError("package evidence path is unsafe")
    if len(raw.encode("utf-8")) > 4_096:
        raise VerificationError("package evidence path exceeds its bound")
    return path


def _safe_child(root: pathlib.Path, relative: pathlib.PurePosixPath) -> pathlib.Path:
    current = root
    for part in relative.parts:
        current = current / part
        try:
            mode = current.lstat().st_mode
        except OSError as error:
            raise VerificationError("package evidence entry is unavailable") from error
        if stat.S_ISLNK(mode):
            raise VerificationError("package evidence contains a symbolic link")
    return current


def _verify_archive(path: pathlib.Path, record: dict[str, Any]) -> None:
    data = _read_regular(path, MAX_ARCHIVE)
    if record.get("bytes") != len(data) or record.get("sha256") != _sha256(data):
        raise VerificationError("corresponding source archive digest differs")
    try:
        decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
        tar_data = decoder.decompress(data, MAX_TAR_BYTES + 1)
        if len(tar_data) > MAX_TAR_BYTES or decoder.unconsumed_tail:
            raise VerificationError("corresponding source archive expansion bound exceeded")
        tar_data += decoder.flush(MAX_TAR_BYTES + 1 - len(tar_data))
    except zlib.error as error:
        raise VerificationError("corresponding source archive is malformed") from error
    if (
        len(tar_data) > MAX_TAR_BYTES
        or not decoder.eof
        or decoder.unused_data
        or decoder.unconsumed_tail
    ):
        raise VerificationError("corresponding source compressed stream differs")
    names: set[str] = set()
    member_records: list[dict[str, Any]] = []
    canonical_members: list[tuple[tarfile.TarInfo, bytes | None]] = []
    count = 0
    total = 0
    try:
        with tarfile.open(fileobj=io.BytesIO(tar_data), mode="r:") as archive:
            for member in archive:
                count += 1
                if count > MAX_ARCHIVE_ENTRIES:
                    raise VerificationError("corresponding source entry bound exceeded")
                name = member.name
                relative = _safe_relative(name.rstrip("/"))
                if relative.parts[0] != "source" or name in names:
                    raise VerificationError("corresponding source path is unsafe")
                names.add(name)
                if not member.isdir() and not member.isfile():
                    raise VerificationError("corresponding source contains an unsupported entry")
                expected_mode = 0o755 if member.isdir() else 0o644
                if (
                    member.mode != expected_mode or member.uid != 0 or member.gid != 0
                    or member.uname != "" or member.gname != "" or member.mtime != 0
                ):
                    raise VerificationError("corresponding source metadata differs")
                total += member.size
                if total > MAX_ARCHIVE_BYTES:
                    raise VerificationError("corresponding source byte bound exceeded")
                if member.isfile():
                    stream = archive.extractfile(member)
                    payload = b"" if stream is None else stream.read(member.size + 1)
                    if len(payload) != member.size:
                        raise VerificationError("corresponding source member is incomplete")
                    member_records.append({
                        "path": relative.as_posix(), "type": "regular", "mode": "0644",
                        "bytes": len(payload), "sha256": _sha256(payload),
                    })
                    canonical = tarfile.TarInfo(relative.as_posix())
                    canonical.size = len(payload)
                    canonical_members.append((canonical, payload))
                else:
                    member_records.append({
                        "path": relative.as_posix() + "/", "type": "directory",
                        "mode": "0755", "bytes": 0, "sha256": None,
                    })
                    canonical = tarfile.TarInfo(relative.as_posix() + "/")
                    canonical.type = tarfile.DIRTYPE
                    canonical_members.append((canonical, None))
    except (OSError, tarfile.TarError) as error:
        raise VerificationError("corresponding source archive is malformed") from error
    if (
        count != record.get("entries")
        or total != record.get("uncompressedBytes")
        or "source" not in names
    ):
        raise VerificationError("corresponding source archive metadata differs")
    canonical_tar = io.BytesIO()
    try:
        with tarfile.open(fileobj=canonical_tar, mode="w", format=tarfile.USTAR_FORMAT) as rebuilt:
            for member, payload in canonical_members:
                member.uid = member.gid = 0
                member.uname = member.gname = ""
                member.mtime = 0
                member.mode = 0o755 if member.isdir() else 0o644
                rebuilt.addfile(member, None if payload is None else io.BytesIO(payload))
    except (OSError, tarfile.TarError, ValueError) as error:
        raise VerificationError("corresponding source archive cannot be normalized") from error
    if canonical_tar.getvalue() != tar_data:
        raise VerificationError("corresponding source tar stream is not canonical")
    member_manifest = record.get("memberManifest")
    canonical = (
        json.dumps(member_records, separators=(",", ":"), sort_keys=True) + "\n"
    ).encode()
    if (
        not isinstance(member_manifest, dict)
        or member_manifest.get("algorithm") != "sha256-canonical-json-v1"
        or member_manifest.get("members") != member_records
        or member_manifest.get("sha256") != _sha256(canonical)
        or member_manifest.get("sha256") != MYSQL_SOURCE_MEMBER_MANIFEST_SHA256
    ):
        raise VerificationError("corresponding source member identity differs")


def verify(
    root: pathlib.Path,
    binary: pathlib.Path,
    ca_bundle: pathlib.Path,
    expected_target: str,
) -> dict[str, Any]:
    try:
        resolved_root = root.resolve(strict=True)
    except OSError as error:
        raise VerificationError("package evidence root or target is unsafe") from error
    if (
        not root.is_absolute() or root.is_symlink() or not resolved_root.is_dir()
        or expected_target not in {"linux-amd64", "linux-arm64"}
    ):
        raise VerificationError("package evidence root or target is unsafe")
    root = resolved_root
    manifest, _ = _load(root / "manifest.json")
    inventory, inventory_raw = _load(root / "target-license-inventory.json")
    notice = _read_regular(root / "THIRD-PARTY-NOTICES.txt", MAX_NOTICE)
    binary_bytes = _read_regular(binary, MAX_BINARY)
    ca_bytes = _read_regular(ca_bundle, 1024 * 1024)
    if (
        manifest.get("schemaVersion") != 1
        or manifest.get("status") != "texts-collected-review-required"
        or manifest.get("caddy") != {
            "version": CADDY_VERSION, "commit": CADDY_COMMIT,
        }
        or manifest.get("unclassifiedLicenseEvidence") != []
        or inventory.get("schemaVersion") != 1
        or inventory.get("status") != "evidence-collected-review-required"
        or inventory.get("caddy") != manifest.get("caddy")
        or inventory.get("consumer") != CONSUMER_RECORD
        or manifest.get("goToolchain") != GO_TOOLCHAIN_RECORD
        or inventory.get("unclassifiedLicenseEvidence") != []
        or inventory.get("missingLicenseEvidence") != []
    ):
        raise VerificationError("Caddy distribution evidence is incomplete")
    targets = manifest.get("targets")
    inventory_targets = inventory.get("targets")
    if (
        not isinstance(targets, list) or len(targets) != 1
        or targets != inventory_targets or targets[0].get("name") != expected_target
        or targets[0].get("goos") != "linux"
        or targets[0].get("goarch") != expected_target.removeprefix("linux-")
        or targets[0].get("cgoEnabled") is not False
        or targets[0].get("buildTags") != []
    ):
        raise VerificationError("Caddy target binding differs")
    modules = inventory.get("modules")
    if (
        not isinstance(modules, list) or not modules
        or inventory.get("moduleCount") != len(modules)
        or manifest.get("moduleCount") != len(modules)
        or manifest.get("licenseFamilyModuleCounts") != inventory.get("licenseFamilyModuleCounts")
    ):
        raise VerificationError("Caddy module inventory is inconsistent")
    seen_modules: set[tuple[str, str | None]] = set()
    observed_families: dict[str, int] = {}
    for module in modules:
        if not isinstance(module, dict):
            raise VerificationError("Caddy module inventory is inconsistent")
        path = module.get("path")
        version = module.get("version")
        families = module.get("licenseFamilies")
        key = (path, version)
        if (
            not isinstance(path, str) or not path or len(path.encode()) > 512
            or (version is not None and not isinstance(version, str))
            or key in seen_modules
            or module.get("targets") != [expected_target]
            or module.get("status") != "observed"
            or not isinstance(families, list) or not families
            or families != sorted(set(families))
            or not isinstance(module.get("licenseAndNoticeFiles"), list)
            or not module["licenseAndNoticeFiles"]
        ):
            raise VerificationError("Caddy module inventory is inconsistent")
        seen_modules.add(key)
        for family in families:
            if not isinstance(family, str):
                raise VerificationError("Caddy module inventory is inconsistent")
            observed_families[family] = observed_families.get(family, 0) + 1
    if dict(sorted(observed_families.items())) != inventory.get("licenseFamilyModuleCounts"):
        raise VerificationError("Caddy module inventory is inconsistent")
    inventory_record = manifest.get("inventory")
    notice_record = manifest.get("combinedNotice")
    binary_record = manifest.get("binary")
    if (
        not isinstance(inventory_record, dict)
        or inventory_record.get("bundlePath") != "target-license-inventory.json"
        or inventory_record.get("bytes") != len(inventory_raw)
        or inventory_record.get("sha256") != _sha256(inventory_raw)
        or not isinstance(notice_record, dict)
        or notice_record.get("bundlePath") != "THIRD-PARTY-NOTICES.txt"
        or notice_record.get("bytes") != len(notice)
        or notice_record.get("sha256") != _sha256(notice)
        or not isinstance(binary_record, dict)
        or binary_record.get("bytes") != len(binary_bytes)
        or binary_record.get("sha256") != _sha256(binary_bytes)
    ):
        raise VerificationError("Caddy package digest binding differs")
    expected_record = EXPECTED_TARGET_RECORDS[expected_target]
    if (
        targets[0].get("packageCount") != expected_record["packageCount"]
        or len(modules) != expected_record["moduleCount"]
        or inventory_record["sha256"] != expected_record["inventorySha256"]
        or notice_record["sha256"] != expected_record["noticeSha256"]
        or binary_record["sha256"] != expected_record["binarySha256"]
    ):
        raise VerificationError("Caddy package differs from reviewed target evidence")
    ca = manifest.get("copiedCaCertificateBundle")
    expected_ca = {
        **CA_RECORD,
        "architecture": "x86_64" if expected_target == "linux-amd64" else "aarch64",
    }
    if (
        ca != expected_ca
        or len(ca_bytes) != expected_ca["bytes"]
        or _sha256(ca_bytes) != expected_ca["sha256"]
    ):
        raise VerificationError("copied CA bundle provenance differs")

    sources = manifest.get("correspondingSources")
    if not isinstance(sources, list) or len(sources) != 1:
        raise VerificationError("Caddy corresponding source set differs")
    source = sources[0]
    archive = source.get("archive") if isinstance(source, dict) else None
    if (
        source.get("path") != "github.com/go-sql-driver/mysql"
        or source.get("version") != "v1.9.3"
        or source.get("licenseText") != "MPL-2.0"
        or not isinstance(archive, dict)
        or archive.get("format") != "tar+gzip"
        or archive.get("root") != "source/"
    ):
        raise VerificationError("Caddy corresponding source binding differs")
    archive_relative = _safe_relative(archive.get("bundlePath"))
    _verify_archive(_safe_child(root, archive_relative), archive)

    expected_files = {
        "manifest.json", "target-license-inventory.json",
        "THIRD-PARTY-NOTICES.txt", archive_relative.as_posix(),
    }
    observed_files: set[str] = set()
    for directory, names, files in os.walk(root, followlinks=False):
        directory_path = pathlib.Path(directory)
        for name in names:
            if (directory_path / name).is_symlink():
                raise VerificationError("package evidence contains a symbolic link")
        for name in files:
            path = directory_path / name
            if path.is_symlink() or not path.is_file():
                raise VerificationError("package evidence contains an unsafe entry")
            observed_files.add(path.relative_to(root).as_posix())
    if observed_files != expected_files:
        raise VerificationError("Caddy package evidence file set differs")
    return {
        "target": expected_target, "moduleCount": len(modules),
        "packageCount": targets[0].get("packageCount"),
        "inventorySha256": inventory_record["sha256"],
        "noticeSha256": notice_record["sha256"],
        "binarySha256": binary_record["sha256"],
        "caBundleSha256": expected_ca["sha256"],
        "unclassifiedLicenseEvidence": [],
    }


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=pathlib.Path)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--ca-bundle", required=True, type=pathlib.Path)
    parser.add_argument("--target", required=True)
    arguments = parser.parse_args(argv)
    try:
        result = verify(
            arguments.root,
            arguments.binary,
            arguments.ca_bundle,
            arguments.target,
        )
    except VerificationError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print("verified packaged Caddy distribution evidence")
    print(json.dumps(result, separators=(",", ":"), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
