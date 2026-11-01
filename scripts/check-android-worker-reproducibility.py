#!/usr/bin/env python3
"""Compare two separately produced worker outputs; never runs a target binary."""
import hashlib
import json
from pathlib import Path
import stat
import sys


def regular(path, limit):
    info = path.lstat()
    if path.resolve() != path or not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= limit:
        raise ValueError("Unsafe or oversized worker evidence: " + path.name)
    return info.st_size


def compare(first, second):
    roots = [Path(value).absolute() for value in (first, second)]
    if roots[0] == roots[1] or any(root.resolve() != root for root in roots):
        raise ValueError("Use two distinct, resolved worker output directories")
    sources = []
    for root in roots:
        path = root / "reports/source-build.json"
        regular(path, 65536)
        sources.append(json.loads(path.read_bytes()))
    if sources[0] != sources[1] or not isinstance(sources[0], dict) or type(sources[0].get("schema")) is not int or sources[0]["schema"] != 1:
        raise ValueError("Worker source/toolchain evidence differs")
    files = {}
    for abi in ("arm64-v8a", "x86_64"):
        for name in ("libcovalentrclone.so", "libengineguardian.so"):
            relative = "jniLibs/" + abi + "/" + name
            records = []
            for root in roots:
                path = root / relative
                size = regular(path, 96 * 1024 * 1024)
                with path.open("rb") as stream:
                    digest = hashlib.file_digest(stream, "sha256").hexdigest()
                records.append({"bytes": size, "sha256": digest})
            if records[0] != records[1]:
                raise ValueError("Worker bytes differ: " + relative + "; first=" + str(records[0]) + "; second=" + str(records[1]))
            files[relative] = records[0]
    return {"schema": 1, "sourceBuild": sources[0], "byteIdentical": True, "files": files}


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: check-android-worker-reproducibility.py FIRST_OUTPUT SECOND_OUTPUT")
    try:
        print(json.dumps(compare(*sys.argv[1:]), indent=2, sort_keys=True))
    except (OSError, ValueError) as error:
        raise SystemExit(str(error))
