#!/usr/bin/env python3
"""Collect bounded license/source evidence for the exact shipped Caddy graph.

This consumes `go list -deps -json` produced with the same target settings as
the Caddy binary. It preserves and classifies observed texts for engineering
review; it does not make a legal conclusion or approve a release.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import re
import stat
import sys
import tarfile
from dataclasses import dataclass
from typing import Any, Iterable


CADDY_VERSION = "v2.11.5-0.20260711231708-b2693fb63a30"
CADDY_COMMIT = "b2693fb63a30e6d7be0972c3645e9a2c0a500e93"
MAIN_MODULE = "covalent.local/caddy"
ROOT_PACKAGE = MAIN_MODULE
GO_VERSION = "go1.26.7"
GO_MOD_SHA256 = "2855429aae39f4f05afa5ac60bebb2dd0a335a4ce5620af7fec94a039e3f44f7"
GO_SUM_SHA256 = "3ac528097eb986c6c39b8a42362c0eb7753f67680f680f3a4c77f9775d6de00a"
MAIN_GO_SHA256 = "148320867ca029601e6bb4d221922f7d1f6817a6c50da2e84761e859e7505800"
COVALENT_LICENSE_SHA256 = "ff9bc316792502655bcae384b8e5a4604da93385fda00552b77d85c334b94acd"
GO_LICENSE_SHA256 = "911f8f5782931320f5b8d1160a76365b83aea6447ee6c04fa6d5591467db9dad"
GO_PATENTS_SHA256 = "96f408bfae65bf137fc2525d3ecb030271c50c1e90799f87abf8846d8dd505cc"
GO_BORING_LICENSE_SHA256 = "56210f826b8f0fbac3160dfe55c97f4019eb6cdda3963b5a0eab8a8bdb62360e"
CA_PACKAGE = "ca-certificates-bundle"
CA_VERSION = "20260611-r0"
CA_ORIGIN = "ca-certificates"
CA_LICENSE = "MPL-2.0 AND MIT"
CA_APORTS_COMMIT = "6e30aafe4fa807ba70797509731f1e7d644dc8f3"
CA_BUNDLE_BYTES = 179_359
CA_BUNDLE_SHA256 = "b8d837841b88bfaa1a0fa827cbca8e2576418dd47c9fc4bb7f1f9d89c83111b9"
SAFE_TARGET = re.compile(r"[A-Za-z0-9_.+-]{1,64}")
SAFE_MODULE = re.compile(r"[A-Za-z0-9._~+/-]{1,512}")
SAFE_VERSION = re.compile(r"v[0-9A-Za-z.+~-]{1,255}")
MODULE_SUM = re.compile(r"h1:[A-Za-z0-9+/]{43}=")
HEX_SHA256 = re.compile(r"[0-9a-f]{64}")
LICENSE_TOKENS = {"LICENSE", "LICENCE", "COPYING"}
NOTICE_TOKENS = {"NOTICE", "COPYRIGHT", "PATENTS", "AUTHORS"}
SOURCE_SUFFIXES = {
    ".c", ".cc", ".cpp", ".go", ".h", ".hpp", ".java", ".js", ".kt",
    ".kts", ".m", ".mm", ".py", ".rs", ".sh", ".swift", ".ts",
}
SPECIAL_LICENSE_TEXTS = {
    # dario.cat/mergo/testdata/license.json is input data whose filename trips
    # the deliberately broad scanner. Retain it, but do not call it a license.
    "0819ea84597f2051f7381c26a67d4119be0770c6ec977ff63c59e26251db2ac0":
        "non-license-test-data",
    # quic-go's logo/trademark policy expressly excludes these uncompiled
    # assets from its MIT license. Retaining the policy avoids an omission.
    "2c8d4ff4244edf09d99dcd428d278e47a059d24f0030b38c1ab38e93b1c8030c":
        "retained-asset-policy",
    # A Windows-only prebuilt Wintun redistribution license is present in the
    # Linux target module source but no Wintun binary is part of this package.
    "183adac21e7d96c508c8fd34d394b7b6708bc81564ad1bad61ab66143a008cd2":
        "retained-unshipped-platform-binary-license",
}


class EvidenceError(Exception):
    """Fixed, non-path-bearing evidence failure."""


@dataclass(frozen=True)
class Limits:
    report_bytes: int = 128 * 1024 * 1024
    report_records: int = 16_384
    modules: int = 2_048
    module_entries: int = 100_000
    all_entries: int = 500_000
    name_bytes: int = 64 * 1024 * 1024
    candidates: int = 16_384
    candidate_bytes: int = 2 * 1024 * 1024
    candidate_total_bytes: int = 96 * 1024 * 1024
    relative_path_bytes: int = 4_096
    path_depth: int = 64
    inventory_bytes: int = 24 * 1024 * 1024
    combined_bytes: int = 16 * 1024 * 1024
    source_entries: int = 100_000
    source_uncompressed_bytes: int = 256 * 1024 * 1024
    source_archive_bytes: int = 64 * 1024 * 1024
    bundle_bytes: int = 96 * 1024 * 1024


@dataclass(frozen=True)
class Target:
    name: str
    goos: str
    goarch: str
    report: pathlib.Path


@dataclass
class Budget:
    entries: int = 0
    name_bytes: int = 0
    candidates: int = 0
    candidate_bytes: int = 0
    source_entries: int = 0
    source_uncompressed_bytes: int = 0
    source_archive_bytes: int = 0


def _read_regular(path: pathlib.Path, maximum: int) -> bytes:
    flags = (
        os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise EvidenceError("required evidence is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
            raise EvidenceError("required evidence is not a bounded regular file")
        chunks: list[bytes] = []
        retained = 0
        while retained <= maximum:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - retained))
            if not chunk:
                break
            chunks.append(chunk)
            retained += len(chunk)
        after = os.fstat(descriptor)
        identity = lambda value: (
            value.st_dev, value.st_ino, value.st_size,
            value.st_mtime_ns, value.st_ctime_ns,
        )
        if retained != before.st_size or retained > maximum or identity(before) != identity(after):
            raise EvidenceError("required evidence changed or exceeded its bound")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _elf_architecture(data: bytes) -> str:
    if (
        len(data) < 20 or data[:4] != b"\x7fELF"
        or data[4] != 2 or data[5] != 1
    ):
        raise EvidenceError("Caddy binary is not a 64-bit little-endian ELF file")
    machine = int.from_bytes(data[18:20], "little")
    architecture = {62: "amd64", 183: "arm64"}.get(machine)
    if architecture is None:
        raise EvidenceError("Caddy binary architecture is unsupported")
    return architecture


def _exact(path: pathlib.Path, expected: str, maximum: int) -> bytes:
    data = _read_regular(path, maximum)
    if _sha256(data) != expected:
        raise EvidenceError("pinned source input differs")
    return data


def _safe_root(path: pathlib.Path) -> pathlib.Path:
    if not path.is_absolute() or path.is_symlink() or not path.is_dir():
        raise EvidenceError("source root is unsafe")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise EvidenceError("source root is unavailable") from error
    if resolved != path:
        raise EvidenceError("source root is not canonical")
    return resolved


def _contained_directory(raw: Any, root: pathlib.Path) -> pathlib.Path:
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise EvidenceError("module source directory is missing")
    candidate = pathlib.Path(raw)
    if not candidate.is_absolute() or ".." in candidate.parts:
        raise EvidenceError("module source directory escapes its evidence root")
    try:
        relative = candidate.relative_to(root)
    except ValueError as error:
        raise EvidenceError("module source directory escapes its evidence root") from error
    current = root
    for part in relative.parts:
        current = current / part
        try:
            mode = current.lstat().st_mode
        except OSError as error:
            raise EvidenceError("module source directory is unavailable") from error
        if stat.S_ISLNK(mode):
            raise EvidenceError("module source path contains a symbolic link")
    if not current.is_dir() or current.resolve(strict=True) != candidate.resolve(strict=True):
        raise EvidenceError("module source directory is unsafe")
    return current


def _read_report(path: pathlib.Path, limits: Limits) -> list[dict[str, Any]]:
    raw = _read_regular(path, limits.report_bytes)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise EvidenceError("target graph is not UTF-8") from error
    decoder = json.JSONDecoder()
    rows: list[dict[str, Any]] = []
    position = 0
    try:
        while position < len(text):
            while position < len(text) and text[position].isspace():
                position += 1
            if position == len(text):
                break
            row, position = decoder.raw_decode(text, position)
            if not isinstance(row, dict):
                raise EvidenceError("target graph record has the wrong shape")
            rows.append(row)
            if len(rows) > limits.report_records:
                raise EvidenceError("target graph record bound exceeded")
    except json.JSONDecodeError as error:
        raise EvidenceError("target graph is malformed") from error
    if not rows:
        raise EvidenceError("target graph is empty")
    return rows


def _candidate_kind(name: str) -> str | None:
    if pathlib.Path(name).suffix.lower() in SOURCE_SUFFIXES:
        return None
    tokens = {token for token in re.split(r"[^A-Z0-9]+", name.upper()) if token}
    if tokens & LICENSE_TOKENS:
        return "license"
    if tokens & NOTICE_TOKENS:
        return "notice"
    return None


def _classify_license(data: bytes, digest: str) -> tuple[list[str], str]:
    # Markdown quote markers split otherwise canonical BSD sentences.
    normalized = b" ".join(data.lower().replace(b">", b" ").split())
    families: list[str] = []
    if all(marker in normalized for marker in (
        b"apache license version 2.0, january 2004",
        b"terms and conditions for use, reproduction, and distribution",
        b"9. accepting warranty or additional liability",
    )):
        families.append("Apache-2.0")
    if all(marker in normalized for marker in (
        b"permission is hereby granted, free of charge, to any person obtaining a copy",
        b"the above copyright notice and this permission notice shall be included",
        b"the software is provided \"as is\"",
    )):
        families.append("MIT")
    if all(marker in normalized for marker in (
        b"redistribution and use in source and binary forms, with or without modification, are permitted",
        b"redistributions of source code must retain",
        b"redistributions in binary form must reproduce",
        b"this software is provided by the copyright holders and contributors",
        b"any express or implied warranties",
    )):
        if b"neither the name" in normalized or b"names of its contributors" in normalized:
            families.append("BSD-3-Clause")
        else:
            families.append("BSD-2-Clause")
    if all(marker in normalized for marker in (
        b"mozilla public license",
        b"version 2.0",
        b"exhibit b - \"incompatible with secondary licenses\" notice",
    )):
        families.append("MPL-2.0")
    if all(marker in normalized for marker in (
        b"cc0 1.0 universal",
        b"creative commons legal code",
        b"statement of purpose",
    )):
        families.append("CC0-1.0")
    if families:
        return sorted(set(families)), "classified-license-text"
    special = SPECIAL_LICENSE_TEXTS.get(digest)
    if special is not None:
        return [], special
    return [], "unclassified"


def _scan_candidates(
    root: pathlib.Path, limits: Limits, budget: Budget
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    stack = [(root, pathlib.PurePosixPath("."))]
    module_entries = 0
    while stack:
        directory, relative_dir = stack.pop()
        try:
            entries = []
            with os.scandir(directory) as iterator:
                for entry in iterator:
                    module_entries += 1
                    budget.entries += 1
                    budget.name_bytes += len(entry.name.encode("utf-8"))
                    if module_entries > limits.module_entries or budget.entries > limits.all_entries:
                        raise EvidenceError("module source entry bound exceeded")
                    if budget.name_bytes > limits.name_bytes:
                        raise EvidenceError("module source name byte bound exceeded")
                    entries.append(entry)
        except OSError as error:
            raise EvidenceError("module source cannot be enumerated completely") from error
        for entry in sorted(entries, key=lambda item: os.fsencode(item.name), reverse=True):
            relative = pathlib.PurePosixPath(entry.name)
            if relative_dir != pathlib.PurePosixPath("."):
                relative = relative_dir / relative
            if len(relative.as_posix().encode("utf-8")) > limits.relative_path_bytes or len(relative.parts) > limits.path_depth:
                raise EvidenceError("module source path bound exceeded")
            try:
                if entry.is_symlink():
                    raise EvidenceError("module source contains a symbolic link")
                if entry.is_dir(follow_symlinks=False):
                    stack.append((pathlib.Path(entry.path), relative))
                    continue
                if not entry.is_file(follow_symlinks=False):
                    raise EvidenceError("module source contains an unsupported entry")
            except OSError as error:
                raise EvidenceError("module source changed during enumeration") from error
            kind = _candidate_kind(entry.name)
            if kind is None:
                continue
            data = _read_regular(pathlib.Path(entry.path), limits.candidate_bytes)
            digest = _sha256(data)
            budget.candidates += 1
            budget.candidate_bytes += len(data)
            if budget.candidates > limits.candidates or budget.candidate_bytes > limits.candidate_total_bytes:
                raise EvidenceError("license candidate aggregate bound exceeded")
            row: dict[str, Any] = {
                "path": relative.as_posix(), "kind": kind,
                "bytes": len(data), "sha256": digest,
            }
            if kind == "license":
                families, disposition = _classify_license(data, digest)
                row["licenseFamilies"] = families
                row["disposition"] = disposition
            result.append(row)
    return sorted(result, key=lambda row: row["path"].encode("utf-8"))


def _module_descriptor(module: dict[str, Any]) -> tuple[str, str | None, dict[str, Any]]:
    path = module.get("Path")
    version = module.get("Version")
    if not isinstance(path, str) or SAFE_MODULE.fullmatch(path) is None:
        raise EvidenceError("compiled package has malformed module metadata")
    if version is not None and (not isinstance(version, str) or SAFE_VERSION.fullmatch(version) is None):
        raise EvidenceError("compiled package has malformed module version")
    replacement = module.get("Replace")
    if replacement is not None and not isinstance(replacement, dict):
        raise EvidenceError("compiled package has malformed replacement metadata")
    if replacement is not None:
        if (
            not isinstance(replacement.get("Path"), str)
            or SAFE_MODULE.fullmatch(replacement["Path"]) is None
            or not isinstance(replacement.get("Version"), str)
            or SAFE_VERSION.fullmatch(replacement["Version"]) is None
        ):
            raise EvidenceError("compiled package replacement provenance is incomplete")
    return path, version, replacement or module


def _write_new(path: pathlib.Path, data: bytes, maximum: int) -> None:
    if len(data) > maximum or path.exists() or path.is_symlink() or not path.parent.is_dir():
        raise EvidenceError("evidence output is unsafe or outside its bound")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(path, flags, 0o444)
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise EvidenceError("evidence output did not complete")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _escape_module(value: str) -> str:
    output = []
    for character in value:
        if character == "!":
            output.append("!!")
        elif "A" <= character <= "Z":
            output.append("!" + character.lower())
        else:
            output.append(character)
    return "".join(output)


def _ca_provenance(installed: pathlib.Path, bundle: pathlib.Path) -> dict[str, Any]:
    database = _read_regular(installed, 4 * 1024 * 1024).decode("utf-8", "strict")
    matches = []
    for stanza in database.split("\n\n"):
        fields: dict[str, str] = {}
        directories: list[str] = []
        files: list[tuple[str, str]] = []
        current_directory = ""
        for line in stanza.splitlines():
            if len(line) < 2 or line[1:2] != ":":
                continue
            key, value = line[0], line[2:]
            if key == "F":
                current_directory = value
                directories.append(value)
            elif key == "R":
                files.append((current_directory, value))
            elif key not in fields:
                fields[key] = value
        if fields.get("P") == CA_PACKAGE:
            matches.append((fields, directories, files))
    if len(matches) != 1:
        raise EvidenceError("CA bundle owner package is missing or ambiguous")
    fields, _, files = matches[0]
    if (
        fields.get("V") != CA_VERSION
        or fields.get("o") != CA_ORIGIN
        or fields.get("L") != CA_LICENSE
        or fields.get("c") != CA_APORTS_COMMIT
        or ("etc/ssl/certs", "ca-certificates.crt") not in files
    ):
        raise EvidenceError("CA bundle package provenance differs")
    data = _exact(bundle, CA_BUNDLE_SHA256, 1024 * 1024)
    if len(data) != CA_BUNDLE_BYTES:
        raise EvidenceError("CA bundle size differs")
    architecture = fields.get("A")
    if architecture not in {"x86_64", "aarch64"}:
        raise EvidenceError("CA bundle package architecture differs")
    return {
        "sourceStage": "golang:1.26.7-alpine3.23",
        "package": CA_PACKAGE,
        "version": CA_VERSION,
        "architecture": architecture,
        "origin": CA_ORIGIN,
        "declaredLicense": CA_LICENSE,
        "aportsCommit": CA_APORTS_COMMIT,
        "bundlePath": "/etc/ssl/certs/ca-certificates.crt",
        "bytes": len(data),
        "sha256": _sha256(data),
        "reviewStatus": "metadata-bound-source-review-required",
    }


def _archive_source(
    root: pathlib.Path, output: pathlib.Path, limits: Limits, budget: Budget
) -> dict[str, Any]:
    entries: list[tuple[pathlib.Path, pathlib.PurePosixPath, bool, int]] = []
    stack = [(root, pathlib.PurePosixPath("source"))]
    while stack:
        directory, relative = stack.pop()
        entries.append((directory, relative, True, 0))
        try:
            children = list(os.scandir(directory))
        except OSError as error:
            raise EvidenceError("corresponding source cannot be enumerated") from error
        for entry in sorted(children, key=lambda item: os.fsencode(item.name), reverse=True):
            child_relative = relative / entry.name
            if len(child_relative.as_posix().encode("utf-8")) > limits.relative_path_bytes or len(child_relative.parts) > limits.path_depth:
                raise EvidenceError("corresponding source path bound exceeded")
            try:
                if entry.is_symlink():
                    raise EvidenceError("corresponding source contains a symbolic link")
                if entry.is_dir(follow_symlinks=False):
                    stack.append((pathlib.Path(entry.path), child_relative))
                    continue
                if not entry.is_file(follow_symlinks=False):
                    raise EvidenceError("corresponding source contains an unsupported entry")
                size = entry.stat(follow_symlinks=False).st_size
            except OSError as error:
                raise EvidenceError("corresponding source changed during enumeration") from error
            entries.append((pathlib.Path(entry.path), child_relative, False, size))
            budget.source_uncompressed_bytes += size
            if budget.source_uncompressed_bytes > limits.source_uncompressed_bytes:
                raise EvidenceError("corresponding source byte bound exceeded")
        if len(entries) > limits.source_entries:
            raise EvidenceError("corresponding source entry bound exceeded")

    payload = io.BytesIO()
    member_records: list[dict[str, Any]] = []
    with gzip.GzipFile(fileobj=payload, mode="wb", filename="", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for path, relative, is_directory, expected_size in sorted(
                entries, key=lambda row: row[1].as_posix().encode("utf-8")
            ):
                info = tarfile.TarInfo(relative.as_posix() + ("/" if is_directory else ""))
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                info.mtime = 0
                info.mode = 0o755 if is_directory else 0o644
                if is_directory:
                    info.type = tarfile.DIRTYPE
                    archive.addfile(info)
                    member_records.append({
                        "path": info.name, "type": "directory", "mode": "0755",
                        "bytes": 0, "sha256": None,
                    })
                else:
                    data = _read_regular(path, min(expected_size, limits.source_uncompressed_bytes))
                    if len(data) != expected_size:
                        raise EvidenceError("corresponding source changed during archival")
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
                    member_records.append({
                        "path": info.name, "type": "regular", "mode": "0644",
                        "bytes": len(data), "sha256": _sha256(data),
                    })
    data = payload.getvalue()
    budget.source_entries += len(entries)
    budget.source_archive_bytes += len(data)
    if budget.source_entries > limits.source_entries or budget.source_archive_bytes > limits.source_archive_bytes:
        raise EvidenceError("corresponding source archive bound exceeded")
    output.parent.mkdir(mode=0o755, exist_ok=True)
    _write_new(output, data, limits.source_archive_bytes)
    member_manifest = (
        json.dumps(member_records, separators=(",", ":"), sort_keys=True) + "\n"
    ).encode()
    return {
        "bundlePath": output.relative_to(output.parents[1]).as_posix(),
        "format": "tar+gzip", "root": "source/", "entries": len(entries),
        "uncompressedBytes": sum(row[3] for row in entries),
        "bytes": len(data), "sha256": _sha256(data),
        "memberManifest": {
            "algorithm": "sha256-canonical-json-v1", "members": member_records,
            "sha256": _sha256(member_manifest),
        },
    }


def _candidate_data(
    module: dict[str, Any], candidate: dict[str, Any], source_root: pathlib.Path,
    module_cache: pathlib.Path, limits: Limits,
) -> bytes:
    root = source_root if module["path"] == MAIN_MODULE else _contained_directory(
        module["directory"], module_cache
    )
    relative = pathlib.PurePosixPath(candidate["path"])
    if relative.is_absolute() or ".." in relative.parts:
        raise EvidenceError("candidate path is unsafe")
    path = root.joinpath(*relative.parts)
    data = _read_regular(path, limits.candidate_bytes)
    if len(data) != candidate["bytes"] or _sha256(data) != candidate["sha256"]:
        raise EvidenceError("candidate changed after inventory")
    return data


def collect(
    source_root: pathlib.Path, module_cache: pathlib.Path, go_root: pathlib.Path,
    targets: list[Target], binary: pathlib.Path, apk_installed: pathlib.Path,
    ca_bundle: pathlib.Path, output: pathlib.Path,
    limits: Limits = Limits(),
) -> dict[str, Any]:
    source_root = _safe_root(source_root)
    module_cache = _safe_root(module_cache)
    go_root = _safe_root(go_root)
    if len(targets) != 1 or output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise EvidenceError("target or output boundary is invalid")
    if [target.name for target in targets] != sorted({target.name for target in targets}):
        raise EvidenceError("target names must be sorted and unique")
    if any(
        SAFE_TARGET.fullmatch(target.name) is None
        or target.goos != "linux"
        or target.goarch not in {"amd64", "arm64"}
        or target.name != f"{target.goos}-{target.goarch}"
        for target in targets
    ):
        raise EvidenceError("target descriptor is malformed")
    _exact(source_root / "go.mod", GO_MOD_SHA256, 256 * 1024)
    _exact(source_root / "go.sum", GO_SUM_SHA256, 2 * 1024 * 1024)
    _exact(source_root / "main.go", MAIN_GO_SHA256, 128 * 1024)
    _exact(source_root / "LICENSE", COVALENT_LICENSE_SHA256, 128 * 1024)
    version_lines = _read_regular(go_root / "VERSION", 128).decode("utf-8", "strict").splitlines()
    if not version_lines or version_lines[0] != GO_VERSION:
        raise EvidenceError("Go source version differs")
    binary_data = _read_regular(binary, 256 * 1024 * 1024)
    if _elf_architecture(binary_data) != targets[0].goarch:
        raise EvidenceError("Caddy binary and target architecture differ")
    ca_provenance = _ca_provenance(apk_installed, ca_bundle)

    modules: dict[tuple[str, str | None], dict[str, Any]] = {}
    standard: dict[str, list[str]] = {}
    package_counts: dict[str, int] = {}
    for target in targets:
        rows = _read_report(target.report, limits)
        imports: set[str] = set()
        standard_imports: set[str] = set()
        for row in rows:
            if row.get("Error") or row.get("DepsErrors"):
                raise EvidenceError("target graph contains a load error")
            import_path = row.get("ImportPath")
            if not isinstance(import_path, str) or not import_path or import_path in imports:
                raise EvidenceError("target graph import binding is malformed")
            imports.add(import_path)
            descriptor = row.get("Module")
            if descriptor is None:
                if row.get("Standard") is not True:
                    raise EvidenceError("target package lacks module or standard evidence")
                standard_imports.add(import_path)
                continue
            if not isinstance(descriptor, dict):
                raise EvidenceError("target module metadata has the wrong shape")
            path, version, effective = _module_descriptor(descriptor)
            main = descriptor.get("Main") is True
            if main != (path == MAIN_MODULE):
                raise EvidenceError("target main module binding differs")
            if main:
                directory = _contained_directory(effective.get("Dir"), source_root)
                if directory != source_root or version is not None:
                    raise EvidenceError("target main source binding differs")
                origin = "pinned-covalent-consumer"
            else:
                if version is None:
                    raise EvidenceError("dependency module has no version")
                directory = _contained_directory(effective.get("Dir"), module_cache)
                origin = "private-gomodcache"
            module_sum = effective.get("Sum")
            if main:
                if module_sum is not None:
                    raise EvidenceError("main module checksum binding differs")
            elif not isinstance(module_sum, str) or MODULE_SUM.fullmatch(module_sum) is None:
                raise EvidenceError("dependency checksum evidence is incomplete")
            key = (path, version)
            replacement = None if effective is descriptor else {
                "path": effective.get("Path"), "version": effective.get("Version")
            }
            binding = (str(directory), replacement, module_sum)
            existing = modules.get(key)
            if existing is None:
                if len(modules) >= limits.modules:
                    raise EvidenceError("compiled module bound exceeded")
                modules[key] = {
                    "path": path, "version": version, "directory": str(directory),
                    "sourceOrigin": origin, "moduleSum": module_sum,
                    "replacement": replacement, "binding": binding,
                    "targets": {target.name},
                }
            elif existing["binding"] != binding:
                raise EvidenceError("compiled module source binding differs by target")
            else:
                existing["targets"].add(target.name)
        if ROOT_PACKAGE not in imports:
            raise EvidenceError("target graph lacks the Caddy consumer root")
        package_counts[target.name] = len(imports)
        standard[target.name] = sorted(standard_imports)

    budget = Budget()
    module_rows: list[dict[str, Any]] = []
    internal_modules: list[dict[str, Any]] = []
    missing: list[str] = []
    unclassified: list[str] = []
    family_counts: dict[str, int] = {}
    for key in sorted(modules, key=lambda value: (value[0].encode("utf-8"), value[1] or "")):
        internal = modules[key]
        candidates = _scan_candidates(pathlib.Path(internal["directory"]), limits, budget)
        root_licenses = [
            candidate for candidate in candidates
            if candidate["kind"] == "license" and "/" not in candidate["path"]
        ]
        families = sorted({
            family for candidate in candidates
            for family in candidate.get("licenseFamilies", [])
        })
        if not root_licenses or not any(candidate.get("licenseFamilies") for candidate in root_licenses):
            missing.append(f"{internal['path']}@{internal['version'] or 'main'}")
        for candidate in candidates:
            if candidate.get("disposition") == "unclassified":
                unclassified.append(
                    f"{internal['path']}@{internal['version'] or 'main'}:{candidate['path']}"
                )
        for family in families:
            family_counts[family] = family_counts.get(family, 0) + 1
        public = {name: internal[name] for name in (
            "path", "version", "sourceOrigin", "moduleSum", "replacement"
        )}
        public.update({
            "targets": sorted(internal["targets"]), "status": "observed",
            "licenseFamilies": families, "licenseAndNoticeFiles": candidates,
        })
        module_rows.append(public)
        internal["candidates"] = candidates
        internal["licenseFamilies"] = families
        internal_modules.append(internal)

    status = "evidence-collected-review-required" if not missing and not unclassified else "incomplete"
    inventory: dict[str, Any] = {
        "schemaVersion": 1, "status": status,
        "scope": "Exact Caddy compiled-target module license/notice evidence; engineering classification only, not legal approval.",
        "caddy": {"version": CADDY_VERSION, "commit": CADDY_COMMIT},
        "consumer": {
            "module": MAIN_MODULE, "goModSha256": GO_MOD_SHA256,
            "goSumSha256": GO_SUM_SHA256, "mainGoSha256": MAIN_GO_SHA256,
            "license": "MIT", "licenseSha256": COVALENT_LICENSE_SHA256,
        },
        "copiedCaCertificateBundle": ca_provenance,
        "targets": [{
            "name": target.name, "goos": target.goos, "goarch": target.goarch,
            "cgoEnabled": False, "buildTags": [],
            "packageCount": package_counts[target.name],
            "standardLibraryPackages": standard[target.name],
        } for target in targets],
        "moduleCount": len(module_rows), "modules": module_rows,
        "missingLicenseEvidence": missing, "unclassifiedLicenseEvidence": unclassified,
        "licenseFamilyModuleCounts": dict(sorted(family_counts.items())),
        "scanBounds": {
            "entriesObserved": budget.entries, "candidateFilesObserved": budget.candidates,
            "candidateBytesObserved": budget.candidate_bytes,
            "maximumEntries": limits.all_entries, "maximumCandidateFiles": limits.candidates,
            "maximumCandidateBytes": limits.candidate_total_bytes,
        },
    }
    if status == "incomplete":
        raise EvidenceError("target license evidence is incomplete")

    output.mkdir(mode=0o755)
    inventory_raw = (json.dumps(inventory, indent=2, sort_keys=True) + "\n").encode()
    _write_new(output / "target-license-inventory.json", inventory_raw, limits.inventory_bytes)

    text_by_digest: dict[str, dict[str, Any]] = {}
    for internal in internal_modules:
        for candidate in internal["candidates"]:
            data = _candidate_data(internal, candidate, source_root, module_cache, limits)
            record = text_by_digest.setdefault(candidate["sha256"], {
                "data": data, "kind": candidate["kind"], "bytes": len(data),
                "references": [], "families": candidate.get("licenseFamilies", []),
                "disposition": candidate.get("disposition"),
            })
            if record["data"] != data or record["kind"] != candidate["kind"]:
                raise EvidenceError("candidate digest collision")
            record["references"].append({
                "module": internal["path"], "version": internal["version"],
                "path": candidate["path"],
            })

    go_evidence = []
    for relative, expected in (("LICENSE", GO_LICENSE_SHA256), ("PATENTS", GO_PATENTS_SHA256)):
        data = _exact(go_root / relative, expected, limits.candidate_bytes)
        go_evidence.append({"sourcePath": relative, "bytes": len(data), "sha256": expected})
        record = text_by_digest.setdefault(expected, {
            "data": data, "kind": "license" if relative == "LICENSE" else "notice",
            "bytes": len(data), "references": [],
            "families": ["BSD-3-Clause"] if relative == "LICENSE" else [],
            "disposition": "classified-license-text" if relative == "LICENSE" else None,
        })
        record["references"].append({"toolchain": GO_VERSION, "path": relative})
    all_standard = set().union(*(set(packages) for packages in standard.values()))
    if "crypto/internal/boring" in all_standard:
        relative = "src/crypto/internal/boring/LICENSE"
        data = _exact(go_root / relative, GO_BORING_LICENSE_SHA256, limits.candidate_bytes)
        go_evidence.append({"sourcePath": relative, "bytes": len(data), "sha256": GO_BORING_LICENSE_SHA256})
        record = text_by_digest.setdefault(GO_BORING_LICENSE_SHA256, {
            "data": data, "kind": "license", "bytes": len(data), "references": [],
            "families": ["OpenSSL-and-SSLeay"], "disposition": "retained-toolchain-license",
        })
        record["references"].append({"toolchain": GO_VERSION, "path": relative})

    source_records = []
    for index, internal in enumerate(internal_modules):
        if "MPL-2.0" not in internal["licenseFamilies"]:
            continue
        archive = _archive_source(
            pathlib.Path(internal["directory"]), output / "sources" / f"{index:04d}-source.tar.gz",
            limits, budget,
        )
        source_path = internal["replacement"]["path"] if internal["replacement"] else internal["path"]
        source_version = internal["replacement"]["version"] if internal["replacement"] else internal["version"]
        if source_version is None:
            raise EvidenceError("MPL corresponding source lacks a version")
        source_records.append({
            "path": internal["path"], "version": internal["version"],
            "sourcePath": source_path, "sourceVersion": source_version,
            "moduleSum": internal["moduleSum"], "licenseText": "MPL-2.0",
            "externalSourceUrl": (
                f"https://proxy.golang.org/{_escape_module(source_path)}/@v/"
                f"{_escape_module(source_version)}.zip"
            ),
            "archive": archive,
        })

    combined_parts = [
        b"Caddy compiled-target third-party notices\n",
        b"Engineering evidence only; classification and release approval remain explicit review steps.\n",
        f"Caddy: {CADDY_VERSION} ({CADDY_COMMIT})\n".encode(),
        f"Go toolchain: {GO_VERSION}\n\n".encode(),
        (
            "Copied CA certificate bundle: " + ca_provenance["package"] + " "
            + ca_provenance["version"] + ", sha256=" + ca_provenance["sha256"]
            + " (separate Alpine source/license review required)\n\n"
        ).encode(),
    ]
    text_rows = []
    for digest in sorted(text_by_digest):
        record = text_by_digest[digest]
        references = sorted(record["references"], key=lambda row: json.dumps(row, sort_keys=True))
        header = (
            "===== retained text " + digest + " =====\n"
            + "References: " + json.dumps(references, separators=(",", ":"), sort_keys=True) + "\n"
            + "Classification: " + ",".join(record["families"] or [record["disposition"] or "notice"]) + "\n\n"
        ).encode()
        combined_parts.extend((header, record["data"], b"\n\n"))
        text_rows.append({
            "sha256": digest, "bytes": record["bytes"], "kind": record["kind"],
            "licenseFamilies": record["families"], "disposition": record["disposition"],
            "references": references,
        })
    combined = b"".join(combined_parts)
    _write_new(output / "THIRD-PARTY-NOTICES.txt", combined, limits.combined_bytes)

    archive_total = sum(record["archive"]["bytes"] for record in source_records)
    bundle_total = len(inventory_raw) + len(combined) + archive_total
    if bundle_total > limits.bundle_bytes:
        raise EvidenceError("Caddy evidence bundle exceeds its aggregate bound")
    manifest = {
        "schemaVersion": 1, "status": "texts-collected-review-required",
        "scope": "Deduplicated byte-exact license/notice texts and MPL-2.0 corresponding-source archives for the supplied Caddy target graph; no legal conclusion or release approval.",
        "caddy": {"version": CADDY_VERSION, "commit": CADDY_COMMIT},
        "binary": {"bytes": len(binary_data), "sha256": _sha256(binary_data)},
        "inventory": {"bundlePath": "target-license-inventory.json", "bytes": len(inventory_raw), "sha256": _sha256(inventory_raw)},
        "combinedNotice": {"bundlePath": "THIRD-PARTY-NOTICES.txt", "bytes": len(combined), "sha256": _sha256(combined)},
        "targets": inventory["targets"], "moduleCount": len(module_rows),
        "licenseFamilyModuleCounts": inventory["licenseFamilyModuleCounts"],
        "unclassifiedLicenseEvidence": [], "retainedUniqueTexts": text_rows,
        "goToolchain": {"version": GO_VERSION, "files": go_evidence},
        "copiedCaCertificateBundle": ca_provenance,
        "correspondingSources": source_records,
        "bounds": {
            "bundleBytes": bundle_total, "maximumBundleBytes": limits.bundle_bytes,
            "sourceEntries": budget.source_entries,
            "sourceUncompressedBytes": budget.source_uncompressed_bytes,
            "sourceArchiveBytes": budget.source_archive_bytes,
        },
        "reviewRequired": [
            "Review every classified or specially retained text before release.",
            "Review platform runtime and copied CA-certificate provenance separately.",
            "Never substitute evidence from another target graph or binary.",
        ],
    }
    manifest_raw = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    _write_new(output / "manifest.json", manifest_raw, limits.inventory_bytes)
    parent = os.open(output, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(parent)
    finally:
        os.close(parent)
    return manifest


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=pathlib.Path)
    parser.add_argument("--module-cache", required=True, type=pathlib.Path)
    parser.add_argument("--go-root", required=True, type=pathlib.Path)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--apk-installed", required=True, type=pathlib.Path)
    parser.add_argument("--ca-bundle", required=True, type=pathlib.Path)
    parser.add_argument(
        "--target", required=True, action="append", nargs=4,
        metavar=("NAME", "GOOS", "GOARCH", "REPORT"),
    )
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args(argv)
    try:
        targets = [Target(name, goos, goarch, pathlib.Path(report)) for name, goos, goarch, report in arguments.target]
        manifest = collect(
            arguments.source_root, arguments.module_cache, arguments.go_root,
            targets, arguments.binary, arguments.apk_installed,
            arguments.ca_bundle, arguments.output,
        )
    except EvidenceError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print("Caddy distribution evidence: texts-collected-review-required")
    print(json.dumps({
        "targets": [{"name": row["name"], "packageCount": row["packageCount"]} for row in manifest["targets"]],
        "moduleCount": manifest["moduleCount"],
        "licenseFamilyModuleCounts": manifest["licenseFamilyModuleCounts"],
        "unclassifiedLicenseEvidence": manifest["unclassifiedLicenseEvidence"],
        "correspondingSourceCount": len(manifest["correspondingSources"]),
        "correspondingSourceBytes": sum(row["archive"]["bytes"] for row in manifest["correspondingSources"]),
        "inventorySha256": manifest["inventory"]["sha256"],
        "noticeManifestStatus": manifest["status"],
        "combinedNoticeSha256": manifest["combinedNotice"]["sha256"],
        "binarySha256": manifest["binary"]["sha256"],
        "copiedCaBundle": {
            "package": manifest["copiedCaCertificateBundle"]["package"],
            "version": manifest["copiedCaCertificateBundle"]["version"],
            "sha256": manifest["copiedCaCertificateBundle"]["sha256"],
            "reviewStatus": manifest["copiedCaCertificateBundle"]["reviewStatus"],
        },
    }, separators=(",", ":"), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
