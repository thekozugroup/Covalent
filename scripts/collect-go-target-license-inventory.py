#!/usr/bin/env python3
"""Inventory bounded license/notice evidence for exact Syncthing target graphs.

This consumes one or more `go list -deps -json` reports produced with the same
GOOS, GOARCH, CGO and build tags as the shipped worker. It does not identify
legal obligations or approve licenses.
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
from dataclasses import dataclass
from typing import Any, Iterable


PINNED_COMMIT = "946e2b83a1f6c6ae119427c09e0a5802940b82ff"
PINNED_VERSION = "v2.1.3"
SOURCE_ARCHIVE_SHA256 = "dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959"
GO_MOD_SHA256 = "a129d6ae9cf20593fab4b1fb04ac09b176c4942d3a4bec9394f9c888fe2d1bd1"
GO_SUM_SHA256 = "7e9606117eca33e9263181a3d0141e403c940c55022a061d8ed9e22d4bda2acd"
MAIN_MODULE = "github.com/syncthing/syncthing"
ROOT_PACKAGE = f"{MAIN_MODULE}/cmd/syncthing"
SAFE_TARGET = re.compile(r"[A-Za-z0-9_.+-]{1,64}")
SAFE_MODULE = re.compile(r"[A-Za-z0-9._~+/-]{1,512}")
SAFE_VERSION = re.compile(r"v[0-9A-Za-z.+~-]{1,255}")
LICENSE_TOKENS = {"LICENSE", "LICENCE", "COPYING"}
NOTICE_TOKENS = {"NOTICE", "COPYRIGHT", "PATENTS", "AUTHORS"}
SOURCE_SUFFIXES = {
    ".c", ".cc", ".cpp", ".go", ".h", ".hpp", ".java", ".js", ".kt",
    ".kts", ".m", ".mm", ".py", ".rs", ".sh", ".swift", ".ts",
}


class EvidenceError(Exception):
    """Fixed, non-path-bearing evidence failure."""


@dataclass(frozen=True)
class Limits:
    report_bytes: int = 128 * 1024 * 1024
    report_records: int = 16_384
    modules: int = 2_048
    module_entries: int = 50_000
    all_entries: int = 250_000
    metadata_name_bytes: int = 32 * 1024 * 1024
    candidate_files: int = 8_192
    candidate_file_bytes: int = 2 * 1024 * 1024
    candidate_total_bytes: int = 64 * 1024 * 1024
    relative_path_bytes: int = 4_096
    path_depth: int = 64
    output_bytes: int = 16 * 1024 * 1024


@dataclass
class ScanBudget:
    entries: int = 0
    name_bytes: int = 0
    candidates: int = 0
    candidate_bytes: int = 0


@dataclass(frozen=True)
class Target:
    name: str
    goos: str
    goarch: str
    cgo_enabled: bool
    tags: tuple[str, ...]
    report: pathlib.Path


def _read_regular(path: pathlib.Path, maximum: int) -> bytes:
    flags = (os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) |
             getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise EvidenceError("required evidence file is unavailable") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise EvidenceError("required evidence file is not a bounded regular file")
        chunks: list[bytes] = []
        retained = 0
        while retained <= maximum:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - retained))
            if not chunk:
                break
            chunks.append(chunk)
            retained += len(chunk)
        final = os.fstat(descriptor)
        if retained != metadata.st_size or retained > maximum or (
            final.st_dev, final.st_ino, final.st_size, final.st_mtime_ns, final.st_ctime_ns
        ) != (
            metadata.st_dev, metadata.st_ino, metadata.st_size,
            metadata.st_mtime_ns, metadata.st_ctime_ns
        ):
            raise EvidenceError("required evidence file changed or exceeded its byte bound")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _read_ndjson(path: pathlib.Path, limits: Limits) -> list[dict[str, Any]]:
    raw = _read_regular(path, limits.report_bytes)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise EvidenceError("target dependency evidence is not UTF-8") from error
    decoder = json.JSONDecoder()
    position = 0
    rows: list[dict[str, Any]] = []
    try:
        while position < len(text):
            while position < len(text) and text[position].isspace():
                position += 1
            if position == len(text):
                break
            row, position = decoder.raw_decode(text, position)
            if not isinstance(row, dict):
                raise EvidenceError("target dependency record has the wrong shape")
            rows.append(row)
            if len(rows) > limits.report_records:
                raise EvidenceError("target dependency record bound exceeded")
    except json.JSONDecodeError as error:
        raise EvidenceError("target dependency evidence is malformed") from error
    if not rows:
        raise EvidenceError("target dependency evidence is empty")
    return rows


def _canonical_root(path: pathlib.Path) -> pathlib.Path:
    if not path.is_absolute() or path.is_symlink() or not path.is_dir():
        raise EvidenceError("module source root is unsafe")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise EvidenceError("module source root is unavailable") from error
    if resolved != path:
        raise EvidenceError("module source root is not canonical")
    return resolved


def _contained_directory(raw: Any, allowed_root: pathlib.Path) -> pathlib.Path:
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise EvidenceError("module source directory is missing")
    candidate = pathlib.Path(raw)
    if not candidate.is_absolute() or ".." in candidate.parts:
        raise EvidenceError("module source directory escapes its evidence root")
    try:
        lexical_relative = candidate.relative_to(allowed_root)
    except ValueError as error:
        raise EvidenceError("module source directory escapes its evidence root") from error
    current = allowed_root
    if current.is_symlink() or not current.is_dir():
        raise EvidenceError("module source root is unsafe")
    for part in lexical_relative.parts:
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


def _candidate_kind(name: str) -> str | None:
    if pathlib.Path(name).suffix.lower() in SOURCE_SUFFIXES:
        return None
    tokens = {token for token in re.split(r"[^A-Z0-9]+", name.upper()) if token}
    if tokens & LICENSE_TOKENS:
        return "license"
    if tokens & NOTICE_TOKENS:
        return "notice"
    return None


def _hash_regular(path: pathlib.Path, maximum: int) -> tuple[int, str]:
    flags = (os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) |
             getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise EvidenceError("license candidate cannot be opened safely") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise EvidenceError("license candidate is not a bounded regular file")
        digest = hashlib.sha256()
        retained = 0
        while retained <= maximum:
            chunk = os.read(descriptor, min(1024 * 1024, maximum + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            digest.update(chunk)
        if retained != metadata.st_size or retained > maximum:
            raise EvidenceError("license candidate changed or exceeded its byte bound")
        final = os.fstat(descriptor)
        if (final.st_dev, final.st_ino, final.st_size, final.st_mtime_ns) != (
            metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns
        ):
            raise EvidenceError("license candidate changed while hashing")
        return retained, digest.hexdigest()
    finally:
        os.close(descriptor)


def _scan_candidates(
    root: pathlib.Path, limits: Limits, budget: ScanBudget
) -> list[dict[str, Any]]:
    candidates: list[dict[str, Any]] = []
    stack: list[tuple[pathlib.Path, pathlib.PurePosixPath]] = [
        (root, pathlib.PurePosixPath("."))
    ]
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
                    if (module_entries > limits.module_entries or
                            budget.entries > limits.all_entries):
                        raise EvidenceError("module source entry bound exceeded")
                    if budget.name_bytes > limits.metadata_name_bytes:
                        raise EvidenceError("module source name byte bound exceeded")
                    entries.append(entry)
        except OSError as error:
            raise EvidenceError("module source cannot be enumerated completely") from error
        for entry in sorted(entries, key=lambda item: os.fsencode(item.name), reverse=True):
            child_relative = pathlib.PurePosixPath(entry.name)
            if relative_dir != pathlib.PurePosixPath("."):
                child_relative = relative_dir / child_relative
            encoded_relative = child_relative.as_posix().encode("utf-8")
            if (len(encoded_relative) > limits.relative_path_bytes or
                    len(child_relative.parts) > limits.path_depth):
                raise EvidenceError("module source path bound exceeded")
            try:
                if entry.is_symlink():
                    raise EvidenceError("module source contains a symbolic link")
                if entry.is_dir(follow_symlinks=False):
                    stack.append((pathlib.Path(entry.path), child_relative))
                    continue
                if not entry.is_file(follow_symlinks=False):
                    raise EvidenceError("module source contains an unsupported entry")
            except OSError as error:
                raise EvidenceError("module source changed during enumeration") from error
            kind = _candidate_kind(entry.name)
            if kind is None:
                continue
            byte_count, digest = _hash_regular(
                pathlib.Path(entry.path), limits.candidate_file_bytes
            )
            budget.candidates += 1
            budget.candidate_bytes += byte_count
            if (budget.candidates > limits.candidate_files or
                    budget.candidate_bytes > limits.candidate_total_bytes):
                raise EvidenceError("license candidate aggregate bound exceeded")
            candidates.append({
                "path": child_relative.as_posix(), "kind": kind,
                "bytes": byte_count, "sha256": digest,
            })
    return sorted(candidates, key=lambda row: row["path"].encode("utf-8"))


def _module_descriptor(module: dict[str, Any]) -> tuple[str, str | None, dict[str, Any]]:
    path = module.get("Path")
    version = module.get("Version")
    if not isinstance(path, str) or SAFE_MODULE.fullmatch(path) is None:
        raise EvidenceError("compiled package has malformed module metadata")
    if version is not None and (
        not isinstance(version, str) or SAFE_VERSION.fullmatch(version) is None
    ):
        raise EvidenceError("compiled package has malformed module version")
    replacement = module.get("Replace")
    if replacement is not None and not isinstance(replacement, dict):
        raise EvidenceError("compiled package has malformed replacement metadata")
    if replacement is not None:
        replacement_path = replacement.get("Path")
        replacement_version = replacement.get("Version")
        if (
            not isinstance(replacement_path, str)
            or SAFE_MODULE.fullmatch(replacement_path) is None
            or not isinstance(replacement_version, str)
            or SAFE_VERSION.fullmatch(replacement_version) is None
        ):
            raise EvidenceError("compiled package replacement provenance is incomplete")
    return path, version, replacement or module


def build_inventory(
    source_root: pathlib.Path,
    module_cache: pathlib.Path,
    targets: list[Target],
    limits: Limits = Limits(),
) -> dict[str, Any]:
    source_root = _canonical_root(source_root)
    module_cache = _canonical_root(module_cache)
    if not targets or len(targets) > 16:
        raise EvidenceError("target count is outside its bound")
    if hashlib.sha256(_read_regular(source_root / "go.mod", 128 * 1024)).hexdigest() != GO_MOD_SHA256:
        raise EvidenceError("pinned source go.mod differs")
    if hashlib.sha256(_read_regular(source_root / "go.sum", 1024 * 1024)).hexdigest() != GO_SUM_SHA256:
        raise EvidenceError("pinned source go.sum differs")
    names = [target.name for target in targets]
    if names != sorted(set(names)):
        raise EvidenceError("target names must be sorted and unique")
    modules: dict[tuple[str, str | None], dict[str, Any]] = {}
    standard_packages: dict[str, set[str]] = {}
    target_package_counts: dict[str, int] = {}
    for target in targets:
        if (
            SAFE_TARGET.fullmatch(target.name) is None
            or SAFE_TARGET.fullmatch(target.goos) is None
            or SAFE_TARGET.fullmatch(target.goarch) is None
            or not target.tags
            or tuple(sorted(set(target.tags))) != target.tags
            or any(SAFE_TARGET.fullmatch(tag) is None for tag in target.tags)
        ):
            raise EvidenceError("target descriptor is malformed")
        rows = _read_ndjson(target.report, limits)
        imports: set[str] = set()
        standard: set[str] = set()
        for row in rows:
            if row.get("Error") or row.get("DepsErrors"):
                raise EvidenceError("target dependency graph contains a load error")
            import_path = row.get("ImportPath")
            if not isinstance(import_path, str) or not import_path:
                raise EvidenceError("target dependency record lacks an import path")
            if import_path in imports:
                raise EvidenceError("target dependency graph contains a duplicate package")
            imports.add(import_path)
            module = row.get("Module")
            if module is None:
                if row.get("Standard") is not True:
                    raise EvidenceError("target package lacks module or standard-library evidence")
                standard.add(import_path)
                continue
            if not isinstance(module, dict):
                raise EvidenceError("target module metadata has the wrong shape")
            path, version, effective = _module_descriptor(module)
            main = module.get("Main") is True
            if main != (path == MAIN_MODULE):
                raise EvidenceError("target main module binding differs")
            if main:
                directory = _contained_directory(effective.get("Dir"), source_root)
                if directory != source_root or version is not None:
                    raise EvidenceError("target main module source binding differs")
                origin = "pinned-source-export"
            else:
                if version is None:
                    raise EvidenceError("target dependency module has no version")
                directory = _contained_directory(effective.get("Dir"), module_cache)
                origin = "private-gomodcache"
            key = (path, version)
            binding = (str(directory), effective.get("Path"), effective.get("Version"))
            existing = modules.get(key)
            if existing is None:
                if len(modules) >= limits.modules:
                    raise EvidenceError("compiled module bound exceeded")
                modules[key] = {
                    "path": path, "version": version, "directory": directory,
                    "origin": origin, "binding": binding, "targets": {target.name},
                    "replacement": None if effective is module else {
                        "path": effective.get("Path"), "version": effective.get("Version")
                    },
                }
            else:
                if existing["binding"] != binding or existing["origin"] != origin:
                    raise EvidenceError("compiled module has conflicting source bindings")
                existing["targets"].add(target.name)
        if ROOT_PACKAGE not in imports:
            raise EvidenceError("target dependency graph lacks Syncthing root package")
        standard_packages[target.name] = standard
        target_package_counts[target.name] = len(imports)

    budget = ScanBudget()
    module_rows: list[dict[str, Any]] = []
    missing: list[str] = []
    for key in sorted(modules, key=lambda item: ((item[0]).encode("utf-8"), item[1] or "")):
        module = modules[key]
        candidates = _scan_candidates(module["directory"], limits, budget)
        root_licenses = [row for row in candidates if "/" not in row["path"] and row["kind"] == "license"]
        module_status = "observed" if root_licenses else "missing-root-license-evidence"
        if module_status != "observed":
            missing.append(f"{module['path']}@{module['version'] or 'main'}")
        row = {
            "path": module["path"], "version": module["version"],
            "sourceOrigin": module["origin"],
            "targets": sorted(module["targets"]), "replacement": module["replacement"],
            "status": module_status, "licenseAndNoticeFiles": candidates,
        }
        if module["path"] == MAIN_MODULE:
            row["declaredLicense"] = "MPL-2.0"
            row["sourceDistributionReview"] = (
                "Review MPL-2.0 notice and corresponding-source obligations for the exact shipped modifications."
            )
        module_rows.append(row)

    unresolved = [
        {
            "component": "Go 1.26.7 toolchain and standard library",
            "status": "release-review-required",
            "reason": "The target graph includes standard-library packages, but the supply-chain output does not retain the pinned Go distribution license and notice files.",
        },
        {
            "component": "Covalent engine guardian",
            "status": "release-review-required",
            "reason": "The separately compiled guardian needs an explicit project license/provenance decision; it is not a Go module.",
        },
        {
            "component": "Platform compiler and runtime link material",
            "status": "release-review-required",
            "reason": "Review final executable link reports separately; the Go package graph does not classify compiler or platform runtime material.",
        },
    ]
    return {
        "schemaVersion": 1,
        "status": "incomplete" if missing else "evidence-collected-review-required",
        "scope": "License/notice filename evidence for modules in exact supplied go list -deps graphs; no legal conclusion or release approval.",
        "source": {
            "version": PINNED_VERSION,
            "commit": PINNED_COMMIT,
            "archiveSha256": SOURCE_ARCHIVE_SHA256,
            "goModSha256": GO_MOD_SHA256,
            "goSumSha256": GO_SUM_SHA256,
        },
        "targets": [
            {
                "abi": target.name,
                "goos": target.goos,
                "goarch": target.goarch,
                "cgoEnabled": target.cgo_enabled,
                "buildTags": list(target.tags),
                "packageCount": target_package_counts[target.name],
                "standardLibraryPackages": sorted(standard_packages[target.name]),
            }
            for target in targets
        ],
        "moduleCount": len(module_rows),
        "modules": module_rows,
        "missingLicenseEvidence": missing,
        "scanBounds": {
            "entriesObserved": budget.entries,
            "candidateFilesObserved": budget.candidates,
            "candidateBytesObserved": budget.candidate_bytes,
            "maximumEntries": limits.all_entries,
            "maximumCandidateFiles": limits.candidate_files,
            "maximumCandidateBytes": limits.candidate_total_bytes,
        },
        "unresolvedReleaseItems": unresolved,
    }


def _write_new(path: pathlib.Path, document: dict[str, Any], maximum: int) -> None:
    if path.exists() or path.is_symlink() or not path.parent.is_dir():
        raise EvidenceError("output must be a new file under an existing directory")
    encoded = (json.dumps(document, sort_keys=True, indent=2) + "\n").encode("utf-8")
    if len(encoded) > maximum:
        raise EvidenceError("license inventory exceeds its output byte bound")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(path, flags, 0o600)
    try:
        view = memoryview(encoded)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise EvidenceError("license inventory write did not complete")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    parent_descriptor = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(parent_descriptor)
    finally:
        os.close(parent_descriptor)


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=pathlib.Path)
    parser.add_argument("--module-cache", required=True, type=pathlib.Path)
    parser.add_argument(
        "--target",
        required=True,
        action="append",
        nargs=6,
        metavar=("NAME", "GOOS", "GOARCH", "CGO", "TAGS", "REPORT"),
        help="exact target: name GOOS GOARCH 0|1 comma-separated-tags report.ndjson",
    )
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args(argv)
    try:
        targets = []
        for name, goos, goarch, cgo, raw_tags, raw_report in arguments.target:
            if cgo not in {"0", "1"} or not raw_tags:
                raise EvidenceError("target descriptor is malformed")
            targets.append(
                Target(
                    name=name,
                    goos=goos,
                    goarch=goarch,
                    cgo_enabled=cgo == "1",
                    tags=tuple(sorted(raw_tags.split(","))),
                    report=pathlib.Path(raw_report),
                )
            )
        inventory = build_inventory(
            arguments.source_root, arguments.module_cache, targets
        )
        _write_new(arguments.output, inventory, Limits().output_bytes)
    except EvidenceError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"Target license inventory: {inventory['status']}")
    print(json.dumps({
        "moduleCount": inventory["moduleCount"],
        "targets": [{"abi": row["abi"], "packageCount": row["packageCount"]}
                    for row in inventory["targets"]],
        "candidateFiles": inventory["scanBounds"]["candidateFilesObserved"],
        "missingLicenseEvidence": inventory["missingLicenseEvidence"],
    }, sort_keys=True))
    return 0 if inventory["status"] != "incomplete" else 1


if __name__ == "__main__":
    raise SystemExit(main())
