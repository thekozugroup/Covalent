#!/usr/bin/env python3
"""Resolve the owned API 37 fixture's installed helper paths and exact hashes.

PackageManager's `pm path` identifies the installed base APK. The caller must
still verify the proposed immutable directory's executables and hashes as the
app UID before changing any permission or running the fixture.
"""

import hashlib
import pathlib
import re
import sys
import zipfile

PACKAGE_APK = re.compile(
    r"package:(/data/app/(?:~~[A-Za-z0-9_=-]{1,128}/)?"
    r"life\.michaelwong\.covalent-[A-Za-z0-9_=-]{1,128}/base\.apk)"
)
HELPERS = {
    "libsyncthing.so": "syncthing-sha256.txt",
    "libengineguardian.so": "engine-guardian-sha256.txt",
}


def resolve(apk: pathlib.Path, package_output: bytes) -> tuple[str, dict[str, str]]:
    if not 0 < len(package_output) <= 4096:
        raise ValueError("installed package response exceeds its bound or is empty")
    lines = package_output.decode("ascii", "strict").splitlines()
    if len(lines) != 1 or (match := PACKAGE_APK.fullmatch(lines[0])) is None:
        raise ValueError("expected the exact Covalent base APK under /data/app")
    directory = str(pathlib.PurePosixPath(match[1]).parent / "lib/x86_64")
    hashes = {}
    with zipfile.ZipFile(apk) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError("APK contains duplicate entries")
        for helper, manifest in HELPERS.items():
            info = archive.getinfo("assets/" + manifest)
            if not 0 < info.file_size <= 4096:
                raise ValueError("helper manifest is empty or oversized")
            records = {}
            for row in archive.read(info).decode("ascii", "strict").splitlines():
                parts = row.split(" ")
                if (len(parts) != 3 or parts[0] in records
                        or parts[0] not in {"arm64-v8a", "x86_64"}
                        or re.fullmatch(r"[0-9a-f]{64}", parts[1]) is None
                        or re.fullmatch(r"[1-9][0-9]{0,8}", parts[2]) is None):
                    raise ValueError("helper manifest is malformed")
                records[parts[0]] = (parts[1], int(parts[2]))
            if set(records) != {"arm64-v8a", "x86_64"}:
                raise ValueError("helper manifest omits a packaged ABI")
            digest, size = records["x86_64"]
            library = archive.getinfo("lib/x86_64/" + helper)
            if not 0 < size <= 64 * 1024 * 1024 or library.file_size != size:
                raise ValueError("helper size differs from its bounded manifest")
            actual = hashlib.sha256()
            retained = 0
            with archive.open(library) as source:
                while chunk := source.read(1024 * 1024):
                    retained += len(chunk)
                    if retained > size:
                        raise ValueError("helper exceeds its declared size")
                    actual.update(chunk)
            if retained != size or actual.hexdigest() != digest:
                raise ValueError("helper bytes differ from their manifest")
            hashes[helper] = digest
    return directory, hashes


def main() -> int:
    try:
        if len(sys.argv) != 2:
            raise ValueError("usage: android-native-library-directory.py debug.apk < pm-path.txt")
        directory, hashes = resolve(pathlib.Path(sys.argv[1]), sys.stdin.buffer.read(4097))
        print(directory)
        for helper, digest in hashes.items():
            print(digest, helper)
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        print(f"Android native-library preflight: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
