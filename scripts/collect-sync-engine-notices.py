#!/usr/bin/env python3
"""Build a bounded notice bundle from an exact Syncthing target inventory.

The input inventory is the output of the target `go list -deps -json` evidence
collector. This tool copies byte-exact license and notice files; it does not
decide legal obligations or approve a release.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import pathlib
import re
import stat
import sys
import tarfile
from dataclasses import dataclass
from typing import Any, Iterable


ENGINE_VERSION = "v2.1.3"
ENGINE_COMMIT = "946e2b83a1f6c6ae119427c09e0a5802940b82ff"
MAIN_MODULE = "github.com/syncthing/syncthing"
GO_VERSION = "go1.26.7"
GO_LICENSE_SHA256 = "911f8f5782931320f5b8d1160a76365b83aea6447ee6c04fa6d5591467db9dad"
GO_PATENTS_SHA256 = "96f408bfae65bf137fc2525d3ecb030271c50c1e90799f87abf8846d8dd505cc"
GO_BORING_LICENSE_SHA256 = "56210f826b8f0fbac3160dfe55c97f4019eb6cdda3963b5a0eab8a8bdb62360e"
SYNCTHING_LICENSE_SHA256 = "3f3d9e0024b1921b067d6f7f88deb4a60cbe7a78e76c64e3f1d7fc3b779b9d04"
SYNCTHING_AUTHORS_SHA256 = "5a0044d13ddf6f013bdd5c2bc419bf45d6123c356567510237e82f304d113d48"
GUARDIAN_SHA256 = "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579"
COVALENT_LICENSE_SHA256 = "ff9bc316792502655bcae384b8e5a4604da93385fda00552b77d85c334b94acd"
OFL_LICENSE_SHA256 = "1d361a8f8e8ce6e68457dcd93fb56e162e6baa3bbb7e7573a290d44399f6b57e"
SAFE_MODULE = re.compile(r"[A-Za-z0-9._~+/-]{1,512}")
SAFE_VERSION = re.compile(r"v[0-9A-Za-z.+~-]{1,255}")
SAFE_TARGET = re.compile(r"[A-Za-z0-9_.+-]{1,64}")
SAFE_TOOLCHAIN_NAME = re.compile(r"[A-Za-z0-9_.+-]{1,64}")
HEX_SHA256 = re.compile(r"[0-9a-f]{64}")
MODULE_SUM = re.compile(r"h1:[A-Za-z0-9+/]{43}=")


class NoticeError(Exception):
    """A fixed, non-path-bearing collection failure."""


@dataclass
class Budget:
    files: int = 0
    bytes: int = 0


@dataclass
class SourceBudget:
    entries: int = 0
    uncompressed_bytes: int = 0
    archive_bytes: int = 0


MAX_INVENTORY_BYTES = 16 * 1024 * 1024
MAX_MODULES = 2_048
MAX_CANDIDATES = 8_192
MAX_CANDIDATE_BYTES = 2 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_RELATIVE_BYTES = 4_096
MAX_DEPTH = 64
MAX_STANDARD_PACKAGES = 4_096
MAX_SOURCE_ENTRIES = 100_000
MAX_SOURCE_NAME_BYTES = 32 * 1024 * 1024
MAX_SOURCE_UNCOMPRESSED_BYTES = 256 * 1024 * 1024
MAX_SOURCE_ARCHIVE_BYTES = 64 * 1024 * 1024
RECOGNIZED_LICENSE_TEXTS = {"MPL-2.0"}


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
        raise NoticeError("required evidence is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
            raise NoticeError("required evidence is not a bounded regular file")
        chunks: list[bytes] = []
        retained = 0
        while retained <= maximum:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            chunks.append(chunk)
        after = os.fstat(descriptor)
        identity = lambda value: (
            value.st_dev,
            value.st_ino,
            value.st_size,
            value.st_mtime_ns,
            value.st_ctime_ns,
        )
        if retained != before.st_size or retained > maximum or identity(before) != identity(after):
            raise NoticeError("required evidence changed during collection")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _recognized_license_texts(data: bytes) -> set[str]:
    """Recognize only texts that need corresponding-source packaging.

    This is deliberately not a general SPDX classifier or legal conclusion.
    """
    normalized = b" ".join(data.lower().split())
    if (
        b"mozilla public license version 2.0" in normalized
        or b"mozilla public license, version 2.0" in normalized
    ):
        return {"MPL-2.0"}
    return set()


def _exact_file(path: pathlib.Path, expected: str, maximum: int) -> bytes:
    data = _read_regular(path, maximum)
    if _sha256(data) != expected:
        raise NoticeError("required notice digest differs")
    return data


def _safe_root(path: pathlib.Path) -> pathlib.Path:
    if not path.is_absolute() or path.is_symlink() or not path.is_dir():
        raise NoticeError("source root is unsafe")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise NoticeError("source root is unavailable") from error
    if resolved != path:
        raise NoticeError("source root is not canonical")
    return resolved


def _safe_child(root: pathlib.Path, raw: str) -> pathlib.Path:
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise NoticeError("notice path is malformed")
    relative = pathlib.PurePosixPath(raw)
    if relative.is_absolute() or ".." in relative.parts or "." in relative.parts:
        raise NoticeError("notice path escapes its source")
    if len(raw.encode("utf-8")) > MAX_RELATIVE_BYTES or len(relative.parts) > MAX_DEPTH:
        raise NoticeError("notice path bound exceeded")
    current = root
    for part in relative.parts:
        current = current / part
        try:
            mode = current.lstat().st_mode
        except OSError as error:
            raise NoticeError("notice path is unavailable") from error
        if stat.S_ISLNK(mode):
            raise NoticeError("notice path contains a symbolic link")
    try:
        current.relative_to(root)
    except ValueError as error:
        raise NoticeError("notice path escapes its source") from error
    return current


def _escape_module(value: str) -> str:
    output: list[str] = []
    for character in value:
        if character == "!":
            output.append("!!")
        elif "A" <= character <= "Z":
            output.extend(("!", character.lower()))
        else:
            output.append(character)
    return "".join(output)


def _module_root(
    caches: list[pathlib.Path], module: str, version: str
) -> pathlib.Path:
    relative = pathlib.Path(f"{_escape_module(module)}@{_escape_module(version)}")
    matches: list[pathlib.Path] = []
    for cache in caches:
        candidate = cache / relative
        if candidate.exists() or candidate.is_symlink():
            matches.append(_safe_root(candidate))
    if not matches:
        raise NoticeError("compiled module source is missing")
    return matches[0]


def _source_identity(metadata: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_size,
        metadata.st_mtime_ns,
    )


def _source_file_digest(path: pathlib.Path, expected: os.stat_result) -> str:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise NoticeError("corresponding source file is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if _source_identity(before) != _source_identity(expected):
            raise NoticeError("corresponding source changed before hashing")
        digest = hashlib.sha256()
        retained = 0
        while retained <= expected.st_size:
            chunk = os.read(descriptor, min(1024 * 1024, expected.st_size + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            digest.update(chunk)
        after = os.fstat(descriptor)
        if retained != expected.st_size or _source_identity(after) != _source_identity(expected):
            raise NoticeError("corresponding source changed while hashing")
        return digest.hexdigest()
    finally:
        os.close(descriptor)


def _source_snapshot(
    root: pathlib.Path,
) -> tuple[tuple[int, int, int, int, int], list[dict[str, Any]], int, int]:
    try:
        root_metadata = root.lstat()
    except OSError as error:
        raise NoticeError("corresponding source is unavailable") from error
    if not stat.S_ISDIR(root_metadata.st_mode) or root.is_symlink():
        raise NoticeError("corresponding source root is unsafe")
    stack: list[tuple[pathlib.Path, pathlib.PurePosixPath]] = [
        (root, pathlib.PurePosixPath("."))
    ]
    rows: list[dict[str, Any]] = []
    total_bytes = 0
    name_bytes = 0
    while stack:
        directory, relative_directory = stack.pop()
        try:
            entries = []
            staged_name_bytes = 0
            with os.scandir(directory) as iterator:
                for entry in iterator:
                    entries.append(entry)
                    staged_name_bytes += len(os.fsencode(entry.name))
                    if len(rows) + len(entries) > MAX_SOURCE_ENTRIES:
                        raise NoticeError("corresponding source entry bound exceeded")
                    if name_bytes + staged_name_bytes > MAX_SOURCE_NAME_BYTES:
                        raise NoticeError("corresponding source name byte bound exceeded")
        except OSError as error:
            raise NoticeError("corresponding source cannot be enumerated") from error
        pending_directories: list[tuple[pathlib.Path, pathlib.PurePosixPath]] = []
        for entry in sorted(entries, key=lambda item: os.fsencode(item.name)):
            relative = pathlib.PurePosixPath(entry.name)
            if relative_directory != pathlib.PurePosixPath("."):
                relative = relative_directory / relative
            try:
                encoded = relative.as_posix().encode("utf-8")
            except UnicodeEncodeError as error:
                raise NoticeError("corresponding source path is not UTF-8") from error
            if len(encoded) > MAX_RELATIVE_BYTES or len(relative.parts) > MAX_DEPTH:
                raise NoticeError("corresponding source path bound exceeded")
            name_bytes += len(encoded)
            if name_bytes > MAX_SOURCE_NAME_BYTES:
                raise NoticeError("corresponding source name byte bound exceeded")
            try:
                metadata = entry.stat(follow_symlinks=False)
            except OSError as error:
                raise NoticeError("corresponding source changed during enumeration") from error
            if stat.S_ISLNK(metadata.st_mode):
                raise NoticeError("corresponding source contains a symbolic link")
            if stat.S_ISDIR(metadata.st_mode):
                kind = "directory"
                pending_directories.append((pathlib.Path(entry.path), relative))
            elif stat.S_ISREG(metadata.st_mode):
                kind = "file"
                total_bytes += metadata.st_size
            else:
                raise NoticeError("corresponding source contains an unsupported entry")
            rows.append(
                {
                    "relative": relative.as_posix(),
                    "kind": kind,
                    "mode": metadata.st_mode,
                    "size": metadata.st_size,
                    "identity": _source_identity(metadata),
                    "sha256": (
                        _source_file_digest(pathlib.Path(entry.path), metadata)
                        if kind == "file"
                        else None
                    ),
                }
            )
            if len(rows) > MAX_SOURCE_ENTRIES:
                raise NoticeError("corresponding source entry bound exceeded")
            if total_bytes > MAX_SOURCE_UNCOMPRESSED_BYTES:
                raise NoticeError("corresponding source byte bound exceeded")
        stack.extend(reversed(pending_directories))
    rows.sort(key=lambda row: os.fsencode(row["relative"]))
    return _source_identity(root_metadata), rows, total_bytes, name_bytes


class _BoundedArchiveWriter:
    def __init__(self, output: Any, maximum: int) -> None:
        self.output = output
        self.maximum = maximum
        self.bytes = 0

    def write(self, data: bytes) -> int:
        if self.bytes + len(data) > self.maximum:
            raise NoticeError("corresponding source archive byte bound exceeded")
        written = self.output.write(data)
        if written != len(data):
            raise NoticeError("corresponding source archive write did not complete")
        self.bytes += written
        return written

    def tell(self) -> int:
        return self.bytes

    def flush(self) -> None:
        self.output.flush()


def _source_archive(
    root: pathlib.Path,
    destination: pathlib.Path,
    budget: SourceBudget,
) -> dict[str, Any]:
    root_identity, rows, source_bytes, name_bytes = _source_snapshot(root)
    if (
        budget.entries + len(rows) > MAX_SOURCE_ENTRIES
        or budget.uncompressed_bytes + source_bytes > MAX_SOURCE_UNCOMPRESSED_BYTES
    ):
        raise NoticeError("corresponding source aggregate bound exceeded")
    destination.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(destination, flags, 0o444)
    except OSError as error:
        raise NoticeError("corresponding source archive cannot be created") from error
    try:
        with os.fdopen(descriptor, "wb", buffering=0, closefd=False) as raw:
            writer = _BoundedArchiveWriter(
                raw, MAX_SOURCE_ARCHIVE_BYTES - budget.archive_bytes
            )
            with gzip.GzipFile(
                filename="", mode="wb", fileobj=writer, mtime=0
            ) as compressed, tarfile.open(
                fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT
            ) as archive:
                root_header = tarfile.TarInfo("source")
                root_header.type = tarfile.DIRTYPE
                root_header.mode = 0o755
                root_header.mtime = 0
                root_header.uid = root_header.gid = 0
                root_header.uname = root_header.gname = ""
                archive.addfile(root_header)
                for row in rows:
                    relative = row["relative"]
                    header = tarfile.TarInfo(f"source/{relative}")
                    header.mode = 0o755 if row["kind"] == "directory" or row["mode"] & 0o111 else 0o644
                    header.mtime = 0
                    header.uid = header.gid = 0
                    header.uname = header.gname = ""
                    if row["kind"] == "directory":
                        header.type = tarfile.DIRTYPE
                        archive.addfile(header)
                        continue
                    header.size = row["size"]
                    source = _safe_child(root, relative)
                    source_flags = (
                        os.O_RDONLY
                        | getattr(os, "O_CLOEXEC", 0)
                        | getattr(os, "O_NOFOLLOW", 0)
                        | getattr(os, "O_NONBLOCK", 0)
                    )
                    try:
                        source_descriptor = os.open(source, source_flags)
                    except OSError as error:
                        raise NoticeError("corresponding source file is unavailable") from error
                    try:
                        before = os.fstat(source_descriptor)
                        if _source_identity(before) != row["identity"]:
                            raise NoticeError("corresponding source changed before file archival")
                        with os.fdopen(source_descriptor, "rb", closefd=False) as file_object:
                            archive.addfile(header, file_object)
                        after = os.fstat(source_descriptor)
                        if _source_identity(after) != row["identity"]:
                            raise NoticeError("corresponding source changed while reading a file")
                    finally:
                        os.close(source_descriptor)
            writer.flush()
            os.fsync(descriptor)
        archive_bytes = os.fstat(descriptor).st_size
    except Exception:
        os.close(descriptor)
        try:
            destination.unlink()
        except OSError:
            pass
        raise
    else:
        os.close(descriptor)
    final_root_identity, final_rows, final_bytes, final_name_bytes = _source_snapshot(root)
    if (
        final_root_identity != root_identity
        or final_rows != rows
        or final_bytes != source_bytes
        or final_name_bytes != name_bytes
    ):
        try:
            destination.unlink()
        except OSError:
            pass
        raise NoticeError("corresponding source tree changed during archival")
    archive_data = _read_regular(destination, MAX_SOURCE_ARCHIVE_BYTES)
    if len(archive_data) != archive_bytes:
        raise NoticeError("corresponding source archive changed after write")
    budget.entries += len(rows)
    budget.uncompressed_bytes += source_bytes
    budget.archive_bytes += archive_bytes
    return {
        "format": "tar+gzip",
        "root": "source/",
        "bundlePath": destination.relative_to(destination.parents[1]).as_posix(),
        "bytes": archive_bytes,
        "sha256": _sha256(archive_data),
        "entries": len(rows),
        "uncompressedBytes": source_bytes,
    }


def _write_new(path: pathlib.Path, data: bytes, mode: int = 0o444) -> None:
    path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags, mode)
    except OSError as error:
        raise NoticeError("notice output cannot be created safely") from error
    try:
        remaining = memoryview(data)
        while remaining:
            count = os.write(descriptor, remaining)
            if count <= 0:
                raise NoticeError("notice output write did not complete")
            remaining = remaining[count:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.chmod(path, mode)


def _combined_notice(
    output: pathlib.Path,
    go_files: list[dict[str, Any]],
    module_rows: list[dict[str, Any]],
    source_archives: list[dict[str, Any]],
    toolchain_notices: list[dict[str, Any]],
) -> bytes:
    sections: list[tuple[str, str]] = []
    for item in go_files:
        sections.append((f"Go {GO_VERSION} / {item['sourcePath']}", item["bundlePath"]))
    for module in module_rows:
        version = module["version"] or ENGINE_COMMIT
        replacement = module.get("replacement")
        replacement_label = ""
        if replacement is not None:
            replacement_label = (
                f" (source {replacement['path']}@{replacement['version']})"
            )
        for item in module["files"]:
            sections.append(
                (
                    f"{module['path']}@{version}{replacement_label} / {item['sourcePath']}",
                    item["bundlePath"],
                )
            )
    sections.extend(
        (
            ("Covalent engine guardian / MIT license", "guardian/Covalent-LICENSE.txt"),
            ("Embedded Fork Awesome assets / OFL-1.1", "supplemental/OFL-1.1.txt"),
        )
    )
    for item in toolchain_notices:
        sections.append((item["label"], item["bundlePath"]))
    combined = bytearray(
        b"Covalent synchronized-folder engine notices\n"
        b"Exact copied texts for the compiled target graph; release review remains required.\n"
    )
    if source_archives:
        combined.extend(
            b"\n===== MPL-2.0 corresponding source =====\n"
            b"Exact source archives are bundled separately from this text, relative to the notice manifest.\n"
        )
        for item in source_archives:
            version = item["version"] or ENGINE_COMMIT
            combined.extend(
                (
                    f"{item['path']}@{version}\n"
                    f"  bundled archive: {item['archive']['bundlePath']}\n"
                    f"  archive SHA-256: {item['archive']['sha256']}\n"
                    f"  archive bytes: {item['archive']['bytes']}\n"
                    f"  external source: {item['externalSourceUrl']}\n"
                ).encode("utf-8")
            )
            if item["moduleSum"] is not None:
                combined.extend(
                    f"  Go module content sum: {item['moduleSum']}\n".encode("utf-8")
                )
    for label, relative in sections:
        data = _read_regular(_safe_child(output, relative), MAX_CANDIDATE_BYTES)
        combined.extend(f"\n===== {label} =====\n".encode("utf-8"))
        combined.extend(data)
        if not data.endswith(b"\n"):
            combined.extend(b"\n")
        if len(combined) > MAX_TOTAL_BYTES:
            raise NoticeError("combined notice byte bound exceeded")
    return bytes(combined)


def _copy_candidate(
    source_root: pathlib.Path,
    source_path: str,
    expected_sha256: str,
    destination: pathlib.Path,
    budget: Budget,
    expected_bytes: int | None = None,
) -> dict[str, Any]:
    if (
        HEX_SHA256.fullmatch(expected_sha256) is None
        or (
            expected_bytes is not None
            and (
                not isinstance(expected_bytes, int)
                or isinstance(expected_bytes, bool)
                or not 0 <= expected_bytes <= MAX_CANDIDATE_BYTES
            )
        )
    ):
        raise NoticeError("notice evidence is malformed")
    data = _exact_file(
        _safe_child(source_root, source_path), expected_sha256, MAX_CANDIDATE_BYTES
    )
    if expected_bytes is not None and len(data) != expected_bytes:
        raise NoticeError("notice byte count differs")
    budget.files += 1
    budget.bytes += len(data)
    if budget.files > MAX_CANDIDATES or budget.bytes > MAX_TOTAL_BYTES:
        raise NoticeError("notice bundle aggregate bound exceeded")
    _write_new(destination, data)
    return {"bytes": len(data), "sha256": expected_sha256}


def _validated_targets(inventory: dict[str, Any]) -> tuple[list[dict[str, Any]], set[str]]:
    targets = inventory.get("targets")
    if not isinstance(targets, list) or not targets:
        raise NoticeError("target inventory has no standard-library evidence")
    validated: list[dict[str, Any]] = []
    names: set[str] = set()
    for target in targets:
        if not isinstance(target, dict):
            raise NoticeError("target inventory row is malformed")
        abi = target.get("abi")
        goarch = target.get("goarch")
        package_count = target.get("packageCount")
        packages = target.get("standardLibraryPackages")
        if (
            not isinstance(abi, str)
            or SAFE_TARGET.fullmatch(abi) is None
            or abi in names
            or not isinstance(goarch, str)
            or SAFE_TARGET.fullmatch(goarch) is None
            or not isinstance(package_count, int)
            or isinstance(package_count, bool)
            or not 1 <= package_count <= 100_000
            or not isinstance(packages, list)
            or not 1 <= len(packages) <= MAX_STANDARD_PACKAGES
            or any(not isinstance(item, str) or not item for item in packages)
            or packages != sorted(set(packages))
        ):
            raise NoticeError("target inventory row is malformed")
        names.add(abi)
        validated.append(target)
    if [target["abi"] for target in validated] != sorted(names):
        raise NoticeError("target inventory order is not canonical")
    return validated, names


def _read_inventory(path: pathlib.Path) -> tuple[dict[str, Any], bytes]:
    raw = _read_regular(path, MAX_INVENTORY_BYTES)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise NoticeError("target inventory is malformed") from error
    if not isinstance(value, dict):
        raise NoticeError("target inventory has the wrong shape")
    source = value.get("source")
    modules = value.get("modules")
    if (
        value.get("schemaVersion") != 1
        or value.get("status") not in {"evidence-collected-review-required", "complete"}
        or value.get("missingLicenseEvidence") != []
        or not isinstance(source, dict)
        or source.get("version") != ENGINE_VERSION
        or source.get("commit") != ENGINE_COMMIT
        or not isinstance(modules, list)
        or not 1 <= len(modules) <= MAX_MODULES
        or value.get("moduleCount") != len(modules)
    ):
        raise NoticeError("target inventory is incomplete or differently pinned")
    return value, raw


def build_bundle(
    inventory_path: pathlib.Path,
    source_root: pathlib.Path,
    module_caches: list[pathlib.Path],
    go_root: pathlib.Path,
    guardian_source: pathlib.Path,
    project_license: pathlib.Path,
    ofl_license: pathlib.Path,
    output: pathlib.Path,
    toolchain_notice_inputs: list[tuple[str, str, pathlib.Path, str]] | None = None,
) -> dict[str, Any]:
    inventory, inventory_raw = _read_inventory(inventory_path)
    source_root = _safe_root(source_root)
    module_caches = [_safe_root(path) for path in module_caches]
    go_root = _safe_root(go_root)
    if not module_caches:
        raise NoticeError("at least one module cache is required")
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise NoticeError("output must be a new directory under an existing parent")
    output.mkdir(mode=0o755)
    budget = Budget()
    source_budget = SourceBudget()
    toolchain_notice_inputs = toolchain_notice_inputs or []
    toolchain_notices: list[dict[str, Any]] = []
    seen_toolchain_names: set[str] = set()
    for label, name, path, expected_digest in toolchain_notice_inputs:
        if (
            not label
            or len(label.encode("utf-8")) > 256
            or any(ord(character) < 0x20 for character in label)
            or SAFE_TOOLCHAIN_NAME.fullmatch(name) is None
            or name in seen_toolchain_names
            or HEX_SHA256.fullmatch(expected_digest) is None
        ):
            raise NoticeError("toolchain notice descriptor is malformed")
        seen_toolchain_names.add(name)
        data = _exact_file(path, expected_digest, MAX_CANDIDATE_BYTES)
        destination = f"toolchain/{name}"
        _write_new(output / destination, data)
        budget.files += 1
        budget.bytes += len(data)
        if budget.files > MAX_CANDIDATES or budget.bytes > MAX_TOTAL_BYTES:
            raise NoticeError("notice bundle aggregate bound exceeded")
        toolchain_notices.append(
            {
                "label": label,
                "sourceName": name,
                "bundlePath": destination,
                "bytes": len(data),
                "sha256": expected_digest,
            }
        )

    version_lines = (
        _read_regular(go_root / "VERSION", 128).decode("utf-8", "strict").splitlines()
    )
    version = version_lines[0] if version_lines else ""
    if version != GO_VERSION:
        raise NoticeError("Go source version differs")
    go_files: list[dict[str, Any]] = []
    for relative, digest in (
        ("LICENSE", GO_LICENSE_SHA256),
        ("PATENTS", GO_PATENTS_SHA256),
    ):
        result = _copy_candidate(
            go_root, relative, digest, output / "go" / relative, budget
        )
        go_files.append({"sourcePath": relative, "bundlePath": f"go/{relative}", **result})

    standard_packages: set[str] = set()
    targets, target_names = _validated_targets(inventory)
    for target in targets:
        packages = target["standardLibraryPackages"]
        standard_packages.update(packages)
    vendor_roots = sorted(
        prefix
        for prefix in (
            "vendor/golang.org/x/crypto",
            "vendor/golang.org/x/net",
            "vendor/golang.org/x/sys",
            "vendor/golang.org/x/text",
        )
        if any(package == prefix or package.startswith(prefix + "/") for package in standard_packages)
    )
    for prefix in vendor_roots:
        for name, digest in (("LICENSE", GO_LICENSE_SHA256), ("PATENTS", GO_PATENTS_SHA256)):
            relative = f"src/{prefix}/{name}"
            destination = f"go/{prefix}/{name}"
            result = _copy_candidate(
                go_root, relative, digest, output / destination, budget
            )
            go_files.append({"sourcePath": relative, "bundlePath": destination, **result})
    if "crypto/internal/boring" in standard_packages:
        relative = "src/crypto/internal/boring/LICENSE"
        destination = "go/crypto-internal-boring-LICENSE.txt"
        result = _copy_candidate(
            go_root,
            relative,
            GO_BORING_LICENSE_SHA256,
            output / destination,
            budget,
        )
        go_files.append({"sourcePath": relative, "bundlePath": destination, **result})

    seen_modules: set[tuple[str, str | None]] = set()
    module_rows: list[dict[str, Any]] = []
    source_archives: list[dict[str, Any]] = []
    for index, module in enumerate(inventory["modules"]):
        if not isinstance(module, dict):
            raise NoticeError("module inventory row is malformed")
        path = module.get("path")
        version_value = module.get("version")
        candidates = module.get("licenseAndNoticeFiles")
        module_targets = module.get("targets")
        replacement = module.get("replacement")
        module_sum = module.get("moduleSum")
        recognized = module.get("recognizedLicenseTexts")
        if (
            not isinstance(path, str)
            or SAFE_MODULE.fullmatch(path) is None
            or (version_value is not None and (
                not isinstance(version_value, str)
                or SAFE_VERSION.fullmatch(version_value) is None
            ))
            or not isinstance(candidates, list)
            or not candidates
            or not isinstance(module_targets, list)
            or not module_targets
            or any(not isinstance(item, str) for item in module_targets)
            or module_targets != sorted(set(module_targets))
            or not set(module_targets).issubset(target_names)
            or module.get("status") != "observed"
            or not isinstance(recognized, list)
            or any(
                not isinstance(item, str) or item not in RECOGNIZED_LICENSE_TEXTS
                for item in recognized
            )
            or recognized != sorted(set(recognized))
        ):
            raise NoticeError("module inventory row is malformed")
        if replacement is not None and (
            not isinstance(replacement, dict)
            or set(replacement) != {"path", "version"}
            or not isinstance(replacement.get("path"), str)
            or SAFE_MODULE.fullmatch(replacement["path"]) is None
            or not isinstance(replacement.get("version"), str)
            or SAFE_VERSION.fullmatch(replacement["version"]) is None
        ):
            raise NoticeError("module replacement evidence is malformed")
        key = (path, version_value)
        if key in seen_modules:
            raise NoticeError("module inventory contains a duplicate")
        seen_modules.add(key)
        if path == MAIN_MODULE:
            if version_value is not None or module_sum is not None:
                raise NoticeError("main module version binding differs")
            root = source_root
        else:
            if (
                version_value is None
                or not isinstance(module_sum, str)
                or MODULE_SUM.fullmatch(module_sum) is None
            ):
                raise NoticeError("dependency module has no version")
            source_path = replacement["path"] if replacement else path
            source_version = replacement["version"] if replacement else version_value
            root = _module_root(module_caches, source_path, source_version)
        candidate_rows: list[dict[str, Any]] = []
        observed_recognized: set[str] = set()
        seen_paths: set[str] = set()
        for candidate in candidates:
            if not isinstance(candidate, dict):
                raise NoticeError("notice inventory row is malformed")
            relative = candidate.get("path")
            kind = candidate.get("kind")
            expected = candidate.get("sha256")
            expected_bytes = candidate.get("bytes")
            if (
                not isinstance(relative, str)
                or relative in seen_paths
                or kind not in {"license", "notice"}
                or not isinstance(expected, str)
            ):
                raise NoticeError("notice inventory row is malformed")
            seen_paths.add(relative)
            bundle_relative = pathlib.PurePosixPath("modules", f"{index:04d}", relative)
            result = _copy_candidate(
                root,
                relative,
                expected,
                output.joinpath(*bundle_relative.parts),
                budget,
                expected_bytes,
            )
            candidate_rows.append(
                {
                    "sourcePath": relative,
                    "bundlePath": bundle_relative.as_posix(),
                    "kind": kind,
                    **result,
                }
            )
            if kind == "license":
                observed_recognized.update(
                    _recognized_license_texts(
                        _read_regular(
                            output.joinpath(*bundle_relative.parts),
                            MAX_CANDIDATE_BYTES,
                        )
                    )
                )
        if sorted(observed_recognized) != recognized:
            raise NoticeError("recognized license text evidence differs")
        module_row = {
            "path": path,
            "version": version_value,
            "replacement": replacement,
            "targets": module_targets,
            "moduleSum": module_sum,
            "recognizedLicenseTexts": recognized,
            "files": candidate_rows,
        }
        if "MPL-2.0" in recognized:
            archive = _source_archive(
                root, output / "sources" / f"{index:04d}-source.tar.gz", source_budget
            )
            budget.files += 1
            budget.bytes += archive["bytes"]
            if budget.files > MAX_CANDIDATES or budget.bytes > MAX_TOTAL_BYTES:
                raise NoticeError("notice bundle aggregate bound exceeded")
            source_path = replacement["path"] if replacement else path
            source_version = replacement["version"] if replacement else version_value
            if path == MAIN_MODULE:
                external_source = (
                    "https://github.com/syncthing/syncthing/tree/" + ENGINE_COMMIT
                )
            else:
                external_source = (
                    "https://proxy.golang.org/"
                    f"{_escape_module(source_path)}/@v/{_escape_module(source_version)}.zip"
                )
            source_record = {
                "path": path,
                "version": version_value,
                "sourcePath": source_path,
                "sourceVersion": source_version,
                "moduleSum": module_sum,
                "licenseText": "MPL-2.0",
                "externalSourceUrl": external_source,
                "archive": archive,
            }
            if path == MAIN_MODULE:
                source_record["sourceCommit"] = ENGINE_COMMIT
            source_archives.append(source_record)
            module_row["correspondingSource"] = source_record
        module_rows.append(
            module_row
        )

    source_license = next(
        (
            file
            for module in module_rows
            if module["path"] == MAIN_MODULE
            for file in module["files"]
            if file["sourcePath"] == "LICENSE"
        ),
        None,
    )
    source_authors = next(
        (
            file
            for module in module_rows
            if module["path"] == MAIN_MODULE
            for file in module["files"]
            if file["sourcePath"] == "AUTHORS"
        ),
        None,
    )
    if (
        source_license is None
        or source_license["sha256"] != SYNCTHING_LICENSE_SHA256
        or source_authors is None
        or source_authors["sha256"] != SYNCTHING_AUTHORS_SHA256
    ):
        raise NoticeError("Syncthing license or author evidence differs")

    guardian = _exact_file(guardian_source, GUARDIAN_SHA256, 128 * 1024)
    covalent_license = _exact_file(project_license, COVALENT_LICENSE_SHA256, 128 * 1024)
    ofl = _exact_file(ofl_license, OFL_LICENSE_SHA256, 128 * 1024)
    _write_new(output / "guardian" / "engine-guardian.c", guardian)
    _write_new(output / "guardian" / "Covalent-LICENSE.txt", covalent_license)
    _write_new(output / "supplemental" / "OFL-1.1.txt", ofl)
    budget.files += 3
    budget.bytes += len(guardian) + len(covalent_license) + len(ofl)
    if budget.files > MAX_CANDIDATES or budget.bytes > MAX_TOTAL_BYTES:
        raise NoticeError("notice bundle aggregate bound exceeded")

    combined = _combined_notice(
        output, go_files, module_rows, source_archives, toolchain_notices
    )
    combined_digest = _sha256(combined)
    budget.files += 1
    budget.bytes += len(combined)
    if budget.files > MAX_CANDIDATES or budget.bytes > MAX_TOTAL_BYTES:
        raise NoticeError("notice bundle aggregate bound exceeded")
    _write_new(output / "THIRD-PARTY-NOTICES.txt", combined)

    manifest: dict[str, Any] = {
        "schemaVersion": 1,
        "status": "texts-collected-review-required",
        "scope": "Byte-exact license and notice texts plus bounded corresponding-source archives for recognized MPL-2.0 texts in the supplied compiled target inventory; no legal conclusion or release approval.",
        "inventorySha256": _sha256(inventory_raw),
        "engine": {"version": ENGINE_VERSION, "commit": ENGINE_COMMIT},
        "targets": targets,
        "modules": module_rows,
        "correspondingSources": source_archives,
        "goToolchain": {
            "version": GO_VERSION,
            "standardLibraryPackages": sorted(standard_packages),
            "files": go_files,
        },
        "guardian": {
            "sourceSha256": GUARDIAN_SHA256,
            "license": "MIT",
            "licenseSha256": COVALENT_LICENSE_SHA256,
            "sourceBundlePath": "guardian/engine-guardian.c",
            "licenseBundlePath": "guardian/Covalent-LICENSE.txt",
        },
        "supplementalLicenses": [
            {
                "reason": "The embedded Fork Awesome font names OFL-1.1 but its upstream candidate file contains only a reference.",
                "license": "OFL-1.1",
                "sha256": OFL_LICENSE_SHA256,
                "bundlePath": "supplemental/OFL-1.1.txt",
            }
        ],
        "combinedNotice": {
            "bundlePath": "THIRD-PARTY-NOTICES.txt",
            "bytes": len(combined),
            "sha256": combined_digest,
        },
        "bounds": {
            "files": budget.files,
            "bytes": budget.bytes,
            "sourceEntries": source_budget.entries,
            "sourceUncompressedBytes": source_budget.uncompressed_bytes,
            "sourceArchiveBytes": source_budget.archive_bytes,
            "maximumSourceEntries": MAX_SOURCE_ENTRIES,
            "maximumSourceUncompressedBytes": MAX_SOURCE_UNCOMPRESSED_BYTES,
            "maximumSourceArchiveBytes": MAX_SOURCE_ARCHIVE_BYTES,
        },
        "reviewRequired": [
            "Classify the copied texts and retain every notice required by each exact target graph.",
            "Review compiler and platform runtime material separately; it is outside the Go module graph.",
            "Never substitute an inventory from another GOOS, GOARCH, CGO, or build-tag combination.",
            "Confirm the recipient-facing source locations remain available for the externally distributed executable.",
        ],
    }
    if toolchain_notices:
        manifest["toolchainNotices"] = toolchain_notices
        manifest["reviewRequired"] = [
            "Classify the copied texts and retain every notice required by each exact target graph.",
            "Confirm the classified final native link inputs match these exact NDK notice files.",
            "Never substitute an inventory from another GOOS, GOARCH, CGO, or build-tag combination.",
            "Confirm the recipient-facing source locations remain available for the externally distributed executable.",
        ]
    encoded = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(encoded) > MAX_INVENTORY_BYTES:
        raise NoticeError("notice manifest bound exceeded")
    _write_new(output / "manifest.json", encoded)
    return manifest


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", required=True, type=pathlib.Path)
    parser.add_argument("--source-root", required=True, type=pathlib.Path)
    parser.add_argument("--module-cache", required=True, action="append", type=pathlib.Path)
    parser.add_argument("--go-root", required=True, type=pathlib.Path)
    parser.add_argument("--guardian-source", required=True, type=pathlib.Path)
    parser.add_argument("--project-license", required=True, type=pathlib.Path)
    parser.add_argument("--ofl-license", required=True, type=pathlib.Path)
    parser.add_argument(
        "--toolchain-notice",
        action="append",
        nargs=4,
        metavar=("LABEL", "NAME", "PATH", "SHA256"),
        default=[],
    )
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args(argv)
    try:
        manifest = build_bundle(
            arguments.inventory,
            arguments.source_root,
            arguments.module_cache,
            arguments.go_root,
            arguments.guardian_source,
            arguments.project_license,
            arguments.ofl_license,
            arguments.output,
            [
                (label, name, pathlib.Path(path), digest)
                for label, name, path, digest in arguments.toolchain_notice
            ],
        )
    except (NoticeError, UnicodeDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(
        json.dumps(
            {
                "status": manifest["status"],
                "modules": len(manifest["modules"]),
                "correspondingSources": len(manifest["correspondingSources"]),
                "files": manifest["bounds"]["files"],
                "bytes": manifest["bounds"]["bytes"],
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
