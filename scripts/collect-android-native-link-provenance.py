#!/usr/bin/env python3
"""Validate bounded Android linker evidence and describe NDK inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shlex
import stat
import sys
from typing import Any, Iterable


MAX_MAP_BYTES = 128 * 1024 * 1024
MAX_TRACE_BYTES = 4 * 1024 * 1024
MAX_DYNAMIC_BYTES = 1024 * 1024
MAX_NDK_METADATA_BYTES = 4 * 1024 * 1024
MAX_NDK_NOTICE_BYTES = 2 * 1024 * 1024
MAX_BINARY_BYTES = 256 * 1024 * 1024
MAX_RESPONSE_BYTES = 8 * 1024 * 1024
MAX_RESPONSE_ARGUMENTS = 65536
MAX_INPUTS = 4096
MAX_LIBRARY_REQUESTS = 1024
SAFE_ABI = re.compile(r"[a-z0-9_-]{1,32}")
SAFE_LIBRARY_NAME = re.compile(r"[A-Za-z0-9_+.-]{1,128}")
NDK_PATH = re.compile(r"(/[^\n\r]+?\.(?:a|o|so)(?:\([^\n\r()]+\))?)(?=:\()")
NEEDED = re.compile(r"Shared library: \[([^\]]+)\]")


class ProvenanceError(ValueError):
    pass


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _write_new(path: pathlib.Path, data: bytes) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    descriptor = os.open(path, flags, 0o644)
    complete = False
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise OSError("provenance manifest write made no progress")
            view = view[written:]
        os.fsync(descriptor)
        complete = True
    finally:
        os.close(descriptor)
        if not complete:
            try:
                path.unlink()
            except FileNotFoundError:
                pass


def _read_regular(path: pathlib.Path, limit: int) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ProvenanceError("required evidence is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size <= 0:
            raise ProvenanceError("required evidence is not a nonempty regular file")
        if before.st_size > limit:
            raise ProvenanceError("required evidence exceeds its byte bound")
        chunks = []
        retained = 0
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, limit + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            if retained > limit:
                raise ProvenanceError("required evidence exceeds its byte bound")
            chunks.append(chunk)
        after = os.fstat(descriptor)
        identity_before = (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        )
        identity_after = (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        )
        if identity_before != identity_after or retained != before.st_size:
            raise ProvenanceError("required evidence changed while being read")
        return b"".join(chunks)
    except OSError as error:
        raise ProvenanceError("required evidence could not be read") from error
    finally:
        os.close(descriptor)


def _read_response(path: pathlib.Path, private_root: pathlib.Path) -> bytes:
    if not path.is_absolute() or ".." in path.parts:
        raise ProvenanceError("external-link response path is unsafe")
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(private_root)
    except (OSError, ValueError) as error:
        raise ProvenanceError("external-link response path is outside the private build") from error
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ProvenanceError("external-link response is unavailable") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_RESPONSE_BYTES:
            raise ProvenanceError("external-link response is invalid or exceeds its byte bound")
        chunks = []
        retained = 0
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, MAX_RESPONSE_BYTES + 1 - retained))
            if not chunk:
                break
            retained += len(chunk)
            if retained > MAX_RESPONSE_BYTES:
                raise ProvenanceError("external-link response exceeds its byte bound")
            chunks.append(chunk)
        after = os.fstat(descriptor)
        if (
            (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
            != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
            or retained != before.st_size
        ):
            raise ProvenanceError("external-link response changed while being read")
        return b"".join(chunks)
    except OSError as error:
        raise ProvenanceError("external-link response could not be read") from error
    finally:
        os.close(descriptor)


def _expanded_go_link_arguments(
    arguments: list[str], private_root: pathlib.Path
) -> list[str]:
    expanded: list[str] = []
    response_seen = False
    for argument in arguments:
        if not argument.startswith("@"):
            expanded.append(argument)
            continue
        if len(argument) == 1 or response_seen:
            raise ProvenanceError("external-link response arguments are ambiguous")
        response_seen = True
        response = _read_response(pathlib.Path(argument[1:]), private_root)
        try:
            text = response.decode("utf-8", "strict")
        except UnicodeDecodeError as error:
            raise ProvenanceError("external-link response is malformed") from error
        if text and not text.endswith("\n"):
            raise ProvenanceError("external-link response is not canonical")
        for line in text.splitlines():
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                raise ProvenanceError("external-link response is malformed") from error
            if not isinstance(value, str) or value.startswith("@"):
                raise ProvenanceError("external-link response is malformed")
            expanded.append(value)
            if len(expanded) > MAX_RESPONSE_ARGUMENTS:
                raise ProvenanceError("external-link response exceeds its argument bound")
    return expanded


def classify_go_link_invocation(arguments: list[str], private_root: pathlib.Path) -> str:
    """Distinguish Go linker probes from its one final external host link."""
    private_root = private_root.resolve(strict=True)
    if not private_root.is_dir():
        raise ProvenanceError("external-link classifier paths are invalid")
    expanded = _expanded_go_link_arguments(arguments, private_root)
    outputs = [expanded[index + 1] for index, value in enumerate(expanded[:-1]) if value == "-o"]
    go_objects = []
    for value in expanded:
        path = pathlib.Path(value)
        if path.name != "go.o":
            continue
        try:
            resolved = path.resolve(strict=True)
            resolved.relative_to(private_root)
        except (OSError, ValueError) as error:
            raise ProvenanceError("final external link has an unsafe Go object") from error
        if not resolved.is_file():
            raise ProvenanceError("final external link Go object is not a regular file")
        go_objects.append(resolved)
    for value in outputs:
        path = pathlib.Path(value)
        if not path.is_absolute():
            raise ProvenanceError("external-link output is not a private absolute path")
        try:
            path.parent.resolve(strict=True).relative_to(private_root)
        except (OSError, ValueError) as error:
            raise ProvenanceError("external-link output is outside the private build") from error
    if not go_objects:
        return "probe"
    if len(set(go_objects)) != 1:
        raise ProvenanceError("final external link lacks one exact private go.o input")
    if len(outputs) != 1:
        raise ProvenanceError("final external-link invocation has an ambiguous output")
    return "final"


def _ndk_revision(source_properties: bytes) -> str:
    try:
        text = source_properties.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ProvenanceError("NDK source properties are malformed") from error
    values = []
    for line in text.splitlines():
        key, separator, value = line.partition("=")
        if separator and key.strip() == "Pkg.Revision":
            values.append(value.strip())
    if len(values) != 1 or not values[0]:
        raise ProvenanceError("NDK source properties are malformed")
    return values[0]


def _split_archive_member(value: str) -> tuple[str, str | None]:
    match = re.fullmatch(r"(.+\.a)\(([^/()]+)\)", value)
    return (match.group(1), match.group(2)) if match else (value, None)


def _classify_input(ndk_root: pathlib.Path, raw_path: str) -> dict[str, str]:
    archive_path, member = _split_archive_member(raw_path)
    path = pathlib.Path(archive_path)
    if not path.is_absolute():
        raise ProvenanceError("link evidence contains an unsafe NDK input path")
    try:
        resolved = path.resolve(strict=True)
        relative = resolved.relative_to(ndk_root).as_posix()
        metadata = resolved.stat()
    except (OSError, ValueError) as error:
        raise ProvenanceError("link evidence contains an out-of-root NDK input") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ProvenanceError("link evidence contains a non-regular NDK input")
    name = resolved.name
    if name in {
        "crtbegin_dynamic.o",
        "crtbegin_so.o",
        "crtend_android.o",
        "crtend_so.o",
    }:
        kind = "android-crt"
    elif name.startswith("libclang_rt.builtins-") and name.endswith(".a"):
        kind = "compiler-rt-builtins"
    elif name in {"libunwind.a", "libunwind.so"}:
        kind = "llvm-unwind"
    elif name in {
        "libc++_static.a",
        "libc++_shared.so",
        "libc++abi.a",
    }:
        kind = "llvm-cxx-runtime"
    elif name in {"libc.so", "libdl.so", "liblog.so", "libm.so"}:
        kind = "android-platform-stub"
    else:
        raise ProvenanceError("link evidence contains an unclassified NDK input")
    result = {"kind": kind, "path": "${NDK}/" + relative}
    if member is not None:
        result["archiveMember"] = member
    return result


def _library_request(argument: str) -> tuple[str, tuple[str, ...]] | None:
    if argument.startswith("-l:"):
        name = argument[3:]
        if SAFE_LIBRARY_NAME.fullmatch(name) is None:
            raise ProvenanceError("Clang trace contains an unsafe library request")
        return (argument, (name,))
    if argument.startswith("-l") and len(argument) > 2:
        name = argument[2:]
        if SAFE_LIBRARY_NAME.fullmatch(name) is None:
            raise ProvenanceError("Clang trace contains an unsafe library request")
        return (argument, (f"lib{name}.a", f"lib{name}.so"))
    if argument.startswith("--library="):
        value = argument.partition("=")[2]
        return _library_request("-l" + value)
    return None


def _driver_inputs(
    trace: str, ndk_root: pathlib.Path
) -> tuple[list[dict[str, str]], list[dict[str, Any]]]:
    invocations: list[list[str]] = []
    for line in trace.splitlines():
        try:
            tokens = shlex.split(line)
        except ValueError as error:
            raise ProvenanceError("Clang driver trace is malformed") from error
        if tokens and pathlib.Path(tokens[0]).name in {"ld.lld", "ld"}:
            invocations.append(tokens)
    if len(invocations) != 1:
        raise ProvenanceError("Clang trace must contain exactly one linker invocation")
    root = str(ndk_root) + os.sep
    values: list[dict[str, str]] = []
    requests: list[dict[str, Any]] = []
    tokens = invocations[0][1:]
    for index, token in enumerate(tokens):
        if token.startswith(root):
            values.append(_classify_input(ndk_root, token))
        request = None
        if token == "-l":
            if index + 1 >= len(tokens):
                raise ProvenanceError("Clang trace contains an incomplete library request")
            request = _library_request("-l" + tokens[index + 1])
        else:
            request = _library_request(token)
        if request is not None:
            argument, candidates = request
            requests.append({"argument": argument, "candidateFiles": list(candidates)})
            if len(requests) > MAX_LIBRARY_REQUESTS:
                raise ProvenanceError("Clang trace exceeds its library request bound")
    if not values:
        raise ProvenanceError("Clang trace contains no concrete NDK link inputs")
    unique = {(row["path"], row.get("archiveMember"), row["kind"]): row for row in values}
    unique_requests = {
        (row["argument"], tuple(row["candidateFiles"])): row for row in requests
    }
    return (
        [unique[key] for key in sorted(unique)],
        [unique_requests[key] for key in sorted(unique_requests)],
    )


def _map_inputs(link_map: str, ndk_root: pathlib.Path) -> list[dict[str, str]]:
    root = str(ndk_root) + os.sep
    values: list[dict[str, str]] = []
    for line in link_map.splitlines():
        matches = list(NDK_PATH.finditer(line))
        if root in line and not matches:
            raise ProvenanceError("link map contains an unparsed NDK input line")
        for match in matches:
            raw = match.group(1).strip()
            start = raw.find(root)
            if start >= 0:
                values.append(_classify_input(ndk_root, raw[start:]))
                if len(values) > MAX_INPUTS:
                    raise ProvenanceError("link map exceeds its NDK input bound")
    unique = {(row["path"], row.get("archiveMember"), row["kind"]): row for row in values}
    return [unique[key] for key in sorted(unique)]


def _dynamic_libraries(report: str, component: str) -> list[str]:
    libraries = sorted(set(NEEDED.findall(report)))
    allowed = {
        "guardian": {"libc.so"},
        "syncthing": {"libc.so", "libdl.so", "liblog.so", "libm.so"},
        "jni": {"libc.so", "libdl.so", "liblog.so", "libm.so"},
    }[component]
    if not libraries or not set(libraries).issubset(allowed):
        raise ProvenanceError("dynamic report contains an unexpected dependency set")
    return libraries


def collect(
    component: str,
    abi: str,
    ndk_root: pathlib.Path,
    expected_revision: str,
    map_path: pathlib.Path,
    trace_path: pathlib.Path,
    dynamic_path: pathlib.Path,
    binary_path: pathlib.Path,
) -> dict[str, Any]:
    if component not in {"syncthing", "guardian", "jni"}:
        raise ProvenanceError("unknown native component")
    if SAFE_ABI.fullmatch(abi) is None:
        raise ProvenanceError("ABI is malformed")
    try:
        root_metadata = ndk_root.lstat()
    except OSError as error:
        raise ProvenanceError("NDK root is unavailable") from error
    if not stat.S_ISDIR(root_metadata.st_mode):
        raise ProvenanceError("NDK root is not a directory")
    ndk_root = ndk_root.resolve(strict=True)

    metadata: dict[str, dict[str, Any]] = {}
    metadata_bytes: dict[str, bytes] = {}
    for name in ("source.properties", "NOTICE", "NOTICE.toolchain"):
        maximum = MAX_NDK_METADATA_BYTES if name == "source.properties" else MAX_NDK_NOTICE_BYTES
        value = _read_regular(ndk_root / name, maximum)
        metadata_bytes[name] = value
        metadata[name] = {"bytes": len(value), "sha256": _sha256(value)}
    if _ndk_revision(metadata_bytes["source.properties"]) != expected_revision:
        raise ProvenanceError("NDK revision differs from the pinned revision")

    map_raw = _read_regular(map_path, MAX_MAP_BYTES)
    trace_raw = _read_regular(trace_path, MAX_TRACE_BYTES)
    dynamic_raw = _read_regular(dynamic_path, MAX_DYNAMIC_BYTES)
    binary_raw = _read_regular(binary_path, MAX_BINARY_BYTES)
    try:
        link_map = map_raw.decode("utf-8", "strict")
        trace = trace_raw.decode("utf-8", "strict")
        dynamic = dynamic_raw.decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ProvenanceError("link evidence is not UTF-8") from error

    requested, library_requests = _driver_inputs(trace, ndk_root)
    contributing = _map_inputs(link_map, ndk_root)
    requested_crt = {pathlib.PurePosixPath(row["path"]).name for row in requested if row["kind"] == "android-crt"}
    expected_crt = (
        {"crtbegin_so.o", "crtend_so.o"}
        if component == "jni"
        else {"crtbegin_dynamic.o", "crtend_android.o"}
    )
    if requested_crt != expected_crt:
        raise ProvenanceError("Clang trace does not contain the exact expected CRT pair")
    if not any(row["kind"] == "android-crt" for row in contributing):
        raise ProvenanceError("link map contains no contributing Android CRT input")
    if not any(row["kind"] == "compiler-rt-builtins" for row in requested):
        raise ProvenanceError("Clang trace does not name compiler-rt builtins")
    requested_paths = {row["path"] for row in requested}
    requested_library_files = {
        candidate
        for request in library_requests
        for candidate in request["candidateFiles"]
    }
    absent = [
        row
        for row in contributing
        if row["kind"] != "android-platform-stub"
        and row["path"] not in requested_paths
        and pathlib.PurePosixPath(row["path"]).name not in requested_library_files
    ]
    if absent:
        summary = ", ".join(
            f'{row["kind"]}:{row["path"]}' for row in absent[:16]
        )
        if len(absent) > 16:
            summary += f", and {len(absent) - 16} more"
        raise ProvenanceError(
            "link map contains an NDK input absent from the driver trace: " + summary
        )

    return {
        "schemaVersion": 1,
        "status": "link-inputs-classified-review-required",
        "component": component,
        "abi": abi,
        "ndk": {"revision": expected_revision, "files": metadata},
        "dynamicLibraries": _dynamic_libraries(dynamic, component),
        "requestedNdkInputs": requested,
        "requestedLinkLibraries": library_requests,
        "contributingNdkInputs": contributing,
        "evidence": {
            "binary": {"bytes": len(binary_raw), "sha256": _sha256(binary_raw)},
            "driverTrace": {"bytes": len(trace_raw), "sha256": _sha256(trace_raw)},
            "linkMap": {"bytes": len(map_raw), "sha256": _sha256(map_raw)},
            "dynamicReport": {"bytes": len(dynamic_raw), "sha256": _sha256(dynamic_raw)},
        },
        "limits": {
            "maximumMapBytes": MAX_MAP_BYTES,
            "maximumTraceBytes": MAX_TRACE_BYTES,
            "maximumDynamicBytes": MAX_DYNAMIC_BYTES,
            "maximumNdkInputs": MAX_INPUTS,
        },
    }


def main(argv: Iterable[str] | None = None) -> int:
    raw_arguments = list(argv) if argv is not None else sys.argv[1:]
    if raw_arguments[:1] == ["--classify-go-link"]:
        classifier = argparse.ArgumentParser(description="Classify a Go external-link invocation.")
        classifier.add_argument("--classify-go-link", action="store_true")
        classifier.add_argument("--private-root", required=True, type=pathlib.Path)
        classifier.add_argument("arguments", nargs=argparse.REMAINDER)
        values = classifier.parse_args(raw_arguments)
        arguments = values.arguments
        if arguments[:1] == ["--"]:
            arguments = arguments[1:]
        try:
            print(classify_go_link_invocation(arguments, values.private_root))
        except (OSError, ProvenanceError) as error:
            print(f"error: {error}", file=sys.stderr)
            return 1
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--component", required=True, choices=("syncthing", "guardian", "jni"))
    parser.add_argument("--abi", required=True)
    parser.add_argument("--ndk-root", required=True, type=pathlib.Path)
    parser.add_argument("--expected-revision", required=True)
    parser.add_argument("--link-map", required=True, type=pathlib.Path)
    parser.add_argument("--driver-trace", required=True, type=pathlib.Path)
    parser.add_argument("--dynamic-report", required=True, type=pathlib.Path)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args(raw_arguments)
    try:
        result = collect(
            arguments.component,
            arguments.abi,
            arguments.ndk_root,
            arguments.expected_revision,
            arguments.link_map,
            arguments.driver_trace,
            arguments.dynamic_report,
            arguments.binary,
        )
        if arguments.output.exists() or arguments.output.is_symlink() or not arguments.output.parent.is_dir():
            raise ProvenanceError("output must be a new file under an existing directory")
        encoded = (json.dumps(result, indent=2, sort_keys=True) + "\n").encode()
        if len(encoded) > 1024 * 1024:
            raise ProvenanceError("provenance manifest exceeds its byte bound")
        _write_new(arguments.output, encoded)
    except (OSError, ProvenanceError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
