#!/usr/bin/env python3
"""Copy a bounded test token into an already-private app-container directory.

Arguments are paths only. The token itself is never accepted through argv or an
environment variable, and this helper intentionally produces no success output.
"""

import os
import stat
import sys


def fail() -> "None":
    print("private UI-test token provisioning failed", file=sys.stderr)
    raise SystemExit(64)


if len(sys.argv) != 3:
    fail()

source_path, target_path = sys.argv[1:]
source_flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
try:
    source_descriptor = os.open(source_path, source_flags)
except OSError:
    fail()

try:
    source_stat = os.fstat(source_descriptor)
    if (
        not stat.S_ISREG(source_stat.st_mode)
        or source_stat.st_uid != os.getuid()
        or stat.S_IMODE(source_stat.st_mode) != 0o600
        or not 32 <= source_stat.st_size <= 513
    ):
        fail()
    token = os.read(source_descriptor, source_stat.st_size + 1)
    if len(token) != source_stat.st_size:
        fail()
finally:
    os.close(source_descriptor)

payload = token[:-1] if token.endswith(b"\n") else token
if not 32 <= len(payload) <= 512 or any(byte < 0x20 or byte > 0x7E for byte in payload):
    fail()

target_parent = os.path.dirname(target_path)
try:
    parent_stat = os.lstat(target_parent)
except OSError:
    fail()
if (
    not stat.S_ISDIR(parent_stat.st_mode)
    or parent_stat.st_uid != os.getuid()
    or stat.S_IMODE(parent_stat.st_mode) != 0o700
):
    fail()

target_flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW
try:
    target_descriptor = os.open(target_path, target_flags, 0o600)
except OSError:
    fail()

try:
    os.fchmod(target_descriptor, 0o600)
    written = os.write(target_descriptor, token)
    if written != len(token):
        fail()
    os.fsync(target_descriptor)
    target_stat = os.fstat(target_descriptor)
    if (
        not stat.S_ISREG(target_stat.st_mode)
        or target_stat.st_uid != os.getuid()
        or stat.S_IMODE(target_stat.st_mode) != 0o600
        or target_stat.st_size != len(token)
    ):
        fail()
finally:
    os.close(target_descriptor)

try:
    directory_descriptor = os.open(
        target_parent,
        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
    )
except OSError:
    fail()
try:
    os.fsync(directory_descriptor)
finally:
    os.close(directory_descriptor)
