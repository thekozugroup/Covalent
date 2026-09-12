#!/usr/bin/env python3
"""Package exact Alpine runtime notices and corresponding source without executing recipes."""
from __future__ import annotations

import argparse
import difflib
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import tarfile
import urllib.parse
import urllib.request

MAX_LOCK = 1024 * 1024
MAX_FILE = 64 * 1024 * 1024
MAX_INPUTS = 128 * 1024 * 1024
MAX_NOTICE = 2 * 1024 * 1024
MAX_SOURCE = 12 * 1024 * 1024
MAX_ARCHIVE_EXPANDED = 256 * 1024 * 1024
MAX_INVENTORY = 4 * 1024 * 1024
LOCK_LIMITS = {'fileBytes': MAX_FILE, 'totalSourceBytes': MAX_INPUTS,
               'sourceBundleBytes': MAX_SOURCE, 'inventoryBytes': MAX_INVENTORY,
               'noticeBytes': MAX_NOTICE}
HEX = re.compile(r"[0-9a-f]{64}\Z")
NAME = re.compile(r"[a-zA-Z0-9_][a-zA-Z0-9._+@-]{0,255}\Z")
PACKAGE_FIELDS = {'P': 'name', 'V': 'version', 'A': 'architecture',
                  'o': 'origin', 'L': 'license', 'c': 'packageCommit'}
PACKAGE_REQUIRED = set(PACKAGE_FIELDS.values()) - {'packageCommit'}
LOCAL_OVERRIDE_FIELDS = {
    'origin', 'packageNames', 'baseVersion', 'localVersion', 'baseAportsCommit',
    'baseRecipeTreeSHA1', 'localRecipeTreeSHA1', 'baseAPKBUILDName',
    'localAPKBUILD', 'addedRecipeFiles', 'transformation', 'provenance',
}


class EvidenceError(Exception):
    pass


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def directory_fd(path: Path, create: bool = False) -> int:
    """Traverse from the filesystem root through held directory descriptors."""
    path = path.absolute()
    fd = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in path.parts[1:]:
            if component in ('.', '..'):
                raise EvidenceError('evidence directory path is not canonical')
            if create:
                try:
                    os.mkdir(component, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        result, fd = fd, -1
        return result
    finally:
        if fd >= 0:
            os.close(fd)


def read_fd(fd: int, limit: int) -> bytes:
    before = os.fstat(fd)
    if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
        raise EvidenceError('evidence is not a bounded regular file')
    with os.fdopen(fd, 'rb', closefd=False) as stream:
        data = stream.read(limit + 1)
    after = os.fstat(fd)
    identity = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    if len(data) != before.st_size or len(data) > limit or identity(before) != identity(after):
        raise EvidenceError('evidence changed during collection')
    return data


def regular(path: Path, limit: int) -> bytes:
    parent = directory_fd(path.absolute().parent)
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        try:
            return read_fd(fd, limit)
        finally:
            os.close(fd)
    finally:
        os.close(parent)


def plain_name(value: object) -> str:
    if not isinstance(value, str) or not NAME.fullmatch(value) or value in ('.', '..'):
        raise EvidenceError('unsafe source name')
    return value


def relative_name(value: object) -> str:
    if not isinstance(value, str) or len(value) > 1024:
        raise EvidenceError('unsafe archive member')
    p = PurePosixPath(value)
    if p.is_absolute() or str(p) != value or any(x in ('.', '..') for x in p.parts):
        raise EvidenceError('unsafe archive member')
    for part in p.parts:
        plain_name(part)
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise EvidenceError('duplicate JSON key')
        result[key] = value
    return result


def parse_database(data: bytes, missing_commit_packages=frozenset()) -> list[dict]:
    records = []
    seen = set()
    for paragraph in data.decode('utf-8').strip().split('\n\n'):
        if not paragraph.strip():
            continue
        row = {}
        for line in paragraph.splitlines():
            if len(line) < 2 or line[1] != ':':
                raise EvidenceError('invalid APK database record')
            if line[0] in PACKAGE_FIELDS:
                key = PACKAGE_FIELDS[line[0]]
                if key in row:
                    raise EvidenceError('duplicate APK identity field')
                row[key] = line[2:]
        if set(row) == PACKAGE_REQUIRED and row.get('name') in missing_commit_packages:
            row['packageCommit'] = None
        elif set(row) != set(PACKAGE_FIELDS.values()):
            raise EvidenceError('incomplete APK identity')
        if row['name'] in seen:
            raise EvidenceError('duplicate installed package')
        seen.add(row['name'])
        records.append(row)
        if len(records) > 1024:
            raise EvidenceError('APK package count exceeds bound')
    if not records:
        raise EvidenceError('empty APK database')
    return sorted(records, key=lambda r: r['name'])


def check_bytes(data: bytes, row: dict) -> None:
    if len(data) != row['bytes'] or sha(data) != row['sha256']:
        raise EvidenceError('source size or SHA256 differs from lock')
    if 'apkbuildSHA512' in row and hashlib.sha512(data).hexdigest() != row['apkbuildSHA512']:
        raise EvidenceError('source SHA512 differs from APKBUILD')
    if 'gitBlob' in row:
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        if blob != row['gitBlob']:
            raise EvidenceError('recipe Git object differs from lock')


def git_tree_digest(entries: list[dict]) -> str:
    payload = b''.join((r['mode'] + ' ' + r['name']).encode() + b'\0' + bytes.fromhex(r['gitBlob'])
                       for r in sorted(entries, key=lambda r: r['name'].encode()))
    return hashlib.sha1(b'tree ' + str(len(payload)).encode() + b'\0' + payload).hexdigest()


def git_blob(data: bytes) -> str:
    return hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()


def expected_packages(lock: dict, arch: str) -> list[dict]:
    result = []
    for row in lock['packages']:
        result.append({
            'name': row['name'], 'version': row['version'], 'origin': row['origin'],
            'license': row['license'],
            'architecture': row.get('architecture', lock['architectures'][arch]),
            'packageCommit': row.get('installedDatabaseCommit', row['aportsCommit']),
        })
    return sorted(result, key=lambda r: r['name'])


def missing_commit_packages(lock: dict) -> set[str]:
    return {r['name'] for r in lock['packages'] if r.get('installedDatabaseCommit', False) is None}


def check_recipe_trees(lock: dict, origins: dict[str, str]) -> None:
    directories = lock['recipeDirectories']
    if not isinstance(directories, list) or len(directories) != len(origins):
        raise EvidenceError('recipe directory tree set is incomplete')
    seen = set()
    for directory in directories:
        origin = plain_name(directory['origin'])
        if origin in seen or directory['aportsCommit'] != origins.get(origin):
            raise EvidenceError('recipe tree origin or commit differs')
        seen.add(origin)
        entries = directory['entries']
        if not isinstance(entries, list) or not 1 <= len(entries) <= 256:
            raise EvidenceError('recipe tree entry bound exceeded')
        names = set()
        for entry in entries:
            name = plain_name(entry['name'])
            if (name in names or entry['mode'] not in ('100644', '100755', '120000')
                    or not re.fullmatch('[0-9a-f]{40}', entry['gitBlob'])):
                raise EvidenceError('recipe tree entry is malformed or duplicated')
            names.add(name)
        if git_tree_digest(entries) != directory['treeSHA1']:
            raise EvidenceError('recipe directory Git tree digest differs')
        locked = {r['name']: r['gitBlob'] for r in lock['files'] if r['origin'] == origin and 'gitBlob' in r}
        if locked != {r['name']: r['gitBlob'] for r in entries}:
            raise EvidenceError('locked files do not cover complete recipe Git tree')
    if seen != origins.keys():
        raise EvidenceError('recipe directory tree set is incomplete')


def check_local_file(row: object, fields: set[str]) -> None:
    if not isinstance(row, dict) or set(row) != fields:
        raise EvidenceError('local source record fields differ')
    relative_name(row['path'])
    if (type(row['bytes']) is not int or not 0 <= row['bytes'] <= MAX_FILE
            or not HEX.fullmatch(row['sha256'])):
        raise EvidenceError('invalid local source bound or digest')
    if 'gitBlob' in fields and not re.fullmatch('[0-9a-f]{40}', row['gitBlob']):
        raise EvidenceError('local recipe Git object is malformed')
    if 'apkbuildSHA512' in fields and not re.fullmatch('[0-9a-f]{128}', row['apkbuildSHA512']):
        raise EvidenceError('local APKBUILD checksum is malformed')


def check_local_overrides(lock: dict, packages: list[dict], origins: dict[str, str]) -> None:
    overrides = lock.get('localRecipeOverrides')
    if not isinstance(overrides, list) or not overrides:
        raise EvidenceError('local recipe override set is missing')
    package_by_origin = {}
    for row in packages:
        package_by_origin.setdefault(row['origin'], []).append(row)
    seen = set()
    total = sum(r['bytes'] for r in lock['files'])
    directories = {d['origin']: d for d in lock['recipeDirectories']}
    for override in overrides:
        if not isinstance(override, dict) or set(override) != LOCAL_OVERRIDE_FIELDS:
            raise EvidenceError('local recipe override fields differ')
        origin = plain_name(override['origin'])
        if origin in seen or origin not in origins:
            raise EvidenceError('duplicate or unknown local recipe override')
        seen.add(origin)
        rows = package_by_origin[origin]
        names = override['packageNames']
        if (not isinstance(names, list) or len(names) != len(set(names))
                or set(names) != {r['name'] for r in rows}):
            raise EvidenceError('local package set differs from origin packages')
        if (origin != 'busybox' or set(names) != {'busybox', 'busybox-binsh', 'ssl_client'}
                or any(r.get('installedDatabaseCommit', object()) is not None for r in rows)
                or any(r['version'] != override['localVersion'] for r in rows)
                or any(r.get('architecture') != ('noarch' if r['name'] == 'busybox-binsh' else None) for r in rows)):
            raise EvidenceError('local installed package metadata differs')
        for key in ('baseVersion', 'localVersion'):
            plain_name(override[key])
        if (override['baseVersion'] == override['localVersion']
                or override['baseAportsCommit'] != origins[origin]
                or override['baseRecipeTreeSHA1'] != directories[origin]['treeSHA1']
                or not re.fullmatch('[0-9a-f]{40}', override['localRecipeTreeSHA1'])):
            raise EvidenceError('local recipe provenance differs')
        plain_name(override['baseAPKBUILDName'])
        if override['baseAPKBUILDName'] == 'APKBUILD':
            raise EvidenceError('local base APKBUILD archive name is ambiguous')
        check_local_file(override['localAPKBUILD'], {'path', 'bytes', 'sha256', 'gitBlob'})
        if override['localAPKBUILD']['path'] != 'aport/APKBUILD':
            raise EvidenceError('local APKBUILD path differs')
        additions = override['addedRecipeFiles']
        if not isinstance(additions, list) or not 1 <= len(additions) <= 16:
            raise EvidenceError('local recipe addition count differs')
        addition_names = set()
        base_names = {r['name'] for r in directories[origin]['entries']}
        for row in additions:
            check_local_file(row, {'path', 'bytes', 'sha256', 'gitBlob', 'apkbuildSHA512'})
            path = PurePosixPath(row['path'])
            if len(path.parts) != 2 or path.parts[0] != 'aport' or path.name in base_names or path.name in addition_names:
                raise EvidenceError('local recipe addition path differs')
            addition_names.add(path.name)
        check_local_file(override['transformation'], {'path', 'bytes', 'sha256'})
        check_local_file(override['provenance'], {'path', 'bytes', 'sha256'})
        if (PurePosixPath(override['transformation']['path']).parent != PurePosixPath('.')
                or PurePosixPath(override['provenance']['path']).parent != PurePosixPath('.')
                or override['transformation']['path'] == override['provenance']['path']):
            raise EvidenceError('local supporting source path differs')
        total += (override['localAPKBUILD']['bytes'] + sum(r['bytes'] for r in additions)
                  + override['transformation']['bytes'] + override['provenance']['bytes'])
        if total > MAX_INPUTS:
            raise EvidenceError('source inputs exceed bound')
    local_names = {r['name'] for r in packages if 'installedDatabaseCommit' in r}
    override_names = {name for o in overrides for name in o['packageNames']}
    if local_names != override_names:
        raise EvidenceError('local package commit exceptions differ')


def load_lock(path: Path) -> tuple[dict, bytes]:
    data = regular(path, MAX_LOCK)
    lock = json.loads(data, object_pairs_hook=unique_object)
    if lock.get('schema') not in (1, 2) or lock.get('architectures') != {'amd64': 'x86_64', 'arm64': 'aarch64'}:
        raise EvidenceError('unsupported runtime lock')
    if (lock.get('limits') != LOCK_LIMITS
            or any(type(v) is not int for v in lock['limits'].values())):
        raise EvidenceError('lock limits differ from enforced bounds')
    for key in ('baseIndexDigest', 'golangIndexDigest'):
        if not isinstance(lock.get(key), str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', lock[key]):
            raise EvidenceError('declared image pin is malformed')
    packages = lock['packages']
    if not isinstance(packages, list) or not 1 <= len(packages) <= 64:
        raise EvidenceError('invalid package lock count')
    names = set()
    origins = {}
    for row in packages:
        required = {'name', 'version', 'origin', 'license', 'aportsCommit'}
        optional = {'installedDatabaseCommit', 'architecture'} if lock['schema'] == 2 else set()
        if not required <= row.keys() or not row.keys() <= required | optional:
            raise EvidenceError('unexpected locked package fields')
        if 'architecture' in row and (row['name'] != 'busybox-binsh' or 'installedDatabaseCommit' not in row):
            raise EvidenceError('architecture override is not a declared local shell package')
        for key in ('name', 'origin', 'version'):
            plain_name(row[key])
        if row['name'] in names or not re.fullmatch('[0-9a-f]{40}', row['aportsCommit']):
            raise EvidenceError('duplicate or malformed locked package')
        names.add(row['name'])
        if not isinstance(row['license'], str) or not 1 <= len(row['license']) <= 128:
            raise EvidenceError('invalid package license')
        if row['origin'] in origins and origins[row['origin']] != row['aportsCommit']:
            raise EvidenceError('origin has conflicting recipes')
        origins[row['origin']] = row['aportsCommit']
    source_origins = lock['sourceBundleOrigins']
    if len(source_origins) != len(set(source_origins)) or not set(source_origins) <= origins.keys():
        raise EvidenceError('invalid source bundle origins')
    required = {r['origin'] for r in packages if 'GPL-' in r['license'] or 'MPL-' in r['license']}
    if not required <= set(source_origins):
        raise EvidenceError('copyleft source origin is missing')
    files = lock['files']
    if not isinstance(files, list) or not 1 <= len(files) <= 512:
        raise EvidenceError('invalid source file count')
    seen = set()
    total = 0
    for row in files:
        key = (plain_name(row['origin']), plain_name(row['name']))
        if key in seen or key[0] not in origins:
            raise EvidenceError('duplicate or unknown source input')
        seen.add(key)
        if type(row['bytes']) is not int or not 0 <= row['bytes'] <= MAX_FILE or not HEX.fullmatch(row['sha256']):
            raise EvidenceError('invalid source bound or digest')
        total += row['bytes']
        if total > MAX_INPUTS:
            raise EvidenceError('source inputs exceed bound')
        url = urllib.parse.urlsplit(row['url'])
        expected_recipe = f"/alpinelinux/aports/{origins[key[0]]}/main/{key[0]}/{urllib.parse.quote(key[1], safe='')}"
        expected_archive = f"/distfiles/v3.23/{key[1]}"
        if (url.scheme != 'https' or url.query or url.fragment or url.username or url.password
                or url.port or not ((url.netloc == 'raw.githubusercontent.com' and url.path == expected_recipe)
                or (url.netloc == 'distfiles.alpinelinux.org' and url.path == expected_archive))):
            raise EvidenceError('source URL is not a reviewed upstream location')
    check_recipe_trees(lock, origins)
    if lock['schema'] == 2:
        check_local_overrides(lock, packages, origins)
    elif 'localRecipeOverrides' in lock:
        raise EvidenceError('local recipe overrides require schema 2')
    if not all((origin, 'APKBUILD') in seen for origin in origins):
        raise EvidenceError('origin recipe is missing')
    if set(lock['originNoticeReferences']) != origins.keys():
        raise EvidenceError('origin notice classification is incomplete')
    notice_ids = set()
    for row in lock['archiveNotices']:
        relative_name(row['member'])
        if ((row['origin'], row['archive']) not in seen or row['member'] in notice_ids
                or not HEX.fullmatch(row['sha256']) or not 0 < row['bytes'] <= MAX_NOTICE):
            raise EvidenceError('invalid archive notice lock')
        notice_ids.add(row['member'])
    for row in lock['supplementalLicenses']:
        plain_name(row['name'])
        if row['name'] in notice_ids or not HEX.fullmatch(row['sha256']) or not 0 < row['bytes'] <= MAX_NOTICE:
            raise EvidenceError('invalid supplemental notice lock')
        notice_ids.add(row['name'])
    for refs in lock['originNoticeReferences'].values():
        if not refs or len(refs) != len(set(refs)) or not set(refs) <= notice_ids:
            raise EvidenceError('unclassified origin notice')
    return lock, data


class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        # Inputs are available at canonical URLs; an unexpected redirect requires review.
        raise EvidenceError('source download redirected from reviewed URL')


def collect_inputs(lock: dict, cache: Path, download: bool) -> dict[tuple[str, str], bytes]:
    cache_fd = directory_fd(cache, create=True)
    inputs = {}
    opener = urllib.request.build_opener(HTTPSRedirect())
    try:
        for row in lock['files']:
            key = (row['origin'], row['name'])
            try:
                os.mkdir(key[0], 0o700, dir_fd=cache_fd)
            except FileExistsError:
                pass
            origin_fd = os.open(key[0], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=cache_fd)
            try:
                try:
                    fd = os.open(key[1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=origin_fd)
                except FileNotFoundError:
                    if not download:
                        raise EvidenceError('locked source is missing from offline cache')
                    with opener.open(row['url'], timeout=40) as response:
                        data = response.read(row['bytes'] + 1)
                    check_bytes(data, row)
                    fd = os.open(key[1], os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                 0o600, dir_fd=origin_fd)
                    try:
                        remaining = memoryview(data)
                        while remaining:
                            written = os.write(fd, remaining)
                            if written <= 0:
                                raise EvidenceError('source cache write did not complete')
                            remaining = remaining[written:]
                        os.fsync(fd)
                        os.lseek(fd, 0, os.SEEK_SET)
                    except BaseException:
                        os.close(fd)
                        raise
                try:
                    data = read_fd(fd, MAX_FILE)
                finally:
                    os.close(fd)
                check_bytes(data, row)
                inputs[key] = data
            finally:
                os.close(origin_fd)
    finally:
        os.close(cache_fd)
    check_recipe_inputs(lock, inputs)
    return inputs


def check_recipe_inputs(lock: dict, inputs: dict) -> None:
    for origin in lock['originNoticeReferences']:
        recipe = inputs[(origin, 'APKBUILD')].decode('utf-8')
        section = re.search(r'^sha512sums="\n(.*?)^"', recipe, re.MULTILINE | re.DOTALL)
        declared = {}
        if section:
            for line in section[1].splitlines():
                match = re.fullmatch(r'([0-9a-f]{128})  (.+)', line)
                if not match or match[2] in declared:
                    raise EvidenceError('APKBUILD source checksums are malformed')
                declared[plain_name(match[2])] = match[1]
        elif re.search(r'^sha512sums=', recipe, re.MULTILINE):
            raise EvidenceError('unsupported APKBUILD checksum format')
        locked = {r['name']: r['apkbuildSHA512'] for r in lock['files']
                  if r['origin'] == origin and 'apkbuildSHA512' in r}
        if declared != locked:
            raise EvidenceError('locked inputs do not cover exact APKBUILD checksums')
        override = next((o for o in lock.get('localRecipeOverrides', []) if o['origin'] == origin), None)
        versions = {override['baseVersion']} if override else {r['version'] for r in lock['packages'] if r['origin'] == origin}
        version = re.search(r'^pkgver=([0-9A-Za-z._+-]+)$', recipe, re.MULTILINE)
        revision = re.search(r'^pkgrel=([0-9]+)$', recipe, re.MULTILINE)
        if not version or not revision or versions != {version[1] + '-r' + revision[1]}:
            raise EvidenceError('recipe version differs from installed package')


def collect_local_inputs(lock: dict, inputs: dict, directory: Path | None) -> dict[str, bytes]:
    """Bind the reviewed recipe delta to its original tree without executing it."""
    local = {}
    for override in lock.get('localRecipeOverrides', []):
        if directory is None:
            raise EvidenceError('local source directory is required')
        origin = override['origin']
        records = [override['localAPKBUILD'], *override['addedRecipeFiles'],
                   override['transformation'], override['provenance']]
        for row in records:
            data = regular(directory / row['path'], MAX_FILE)
            check_bytes(data, row)
            if 'gitBlob' in row and git_blob(data) != row['gitBlob']:
                raise EvidenceError('local recipe Git blob differs')
            if 'apkbuildSHA512' in row and hashlib.sha512(data).hexdigest() != row['apkbuildSHA512']:
                raise EvidenceError('local recipe source checksum differs')
            local[row['path']] = data
        original = inputs[(origin, 'APKBUILD')]
        recipe = local[override['localAPKBUILD']['path']]
        delta = ''.join(difflib.unified_diff(original.decode().splitlines(keepends=True),
                       recipe.decode().splitlines(keepends=True),
                       fromfile=override['baseAPKBUILDName'], tofile='aport/APKBUILD')).encode()
        if delta != local[override['transformation']['path']]:
            raise EvidenceError('local recipe transformation differs from exact base and result')
        base = next(d for d in lock['recipeDirectories'] if d['origin'] == origin)
        entries = [dict(r) for r in base['entries']]
        next(r for r in entries if r['name'] == 'APKBUILD')['gitBlob'] = override['localAPKBUILD']['gitBlob']
        entries.extend({'name': PurePosixPath(r['path']).name, 'mode': '100644', 'gitBlob': r['gitBlob']}
                       for r in override['addedRecipeFiles'])
        if git_tree_digest(entries) != override['localRecipeTreeSHA1']:
            raise EvidenceError('local recipe tree differs from exact base and additions')
        # Reuse the complete checksum/version checks with the changed recipe.
        effective = {**lock, 'localRecipeOverrides': [], 'files': [*lock['files'],
                     *[{**r, 'origin': origin, 'name': PurePosixPath(r['path']).name}
                       for r in override['addedRecipeFiles']]]}
        changed = {**inputs, (origin, 'APKBUILD'): recipe}
        check_recipe_inputs(effective, changed)
    return local


def source_records(lock: dict) -> tuple[list[dict], dict[str, str]]:
    rows = [dict(r) for r in lock['files'] if r['origin'] in lock['sourceBundleOrigins']]
    modes = {f"{d['origin']}/{r['name']}": r['mode'] for d in lock['recipeDirectories'] for r in d['entries']}
    if lock['schema'] == 1:
        return rows, modes
    for row in rows:
        row['archivePath'] = f"{row['origin']}/{row['name']}"
    for override in lock['localRecipeOverrides']:
        origin = override['origin']
        for row in rows:
            if row['origin'] == origin and 'gitBlob' in row:
                old = row['archivePath']
                row['archivePath'] = f"{origin}/" + (override['baseAPKBUILDName'] if row['name'] == 'APKBUILD'
                                                    else 'aport/' + row['name'])
                modes[row['archivePath']] = modes.pop(old)
        for row in [override['localAPKBUILD'], *override['addedRecipeFiles'],
                    override['transformation'], override['provenance']]:
            rows.append({**row, 'origin': origin, 'archivePath': f"{origin}/{row['path']}"})
            modes[f"{origin}/{row['path']}"] = '100644'
    if len({r['archivePath'] for r in rows}) != len(rows):
        raise EvidenceError('local source archive paths overlap')
    return rows, modes


def read_notices(lock: dict, inputs: dict, directory: Path) -> dict[str, bytes]:
    notices = {}
    groups = {}
    for row in lock['archiveNotices']:
        groups.setdefault((row['origin'], row['archive']), {})[row['member']] = row
    for key, wanted in groups.items():
        expanded = 0
        count = 0
        seen = set()
        # Never extract an archive to disk, follow links, or execute an APKBUILD.
        with tarfile.open(fileobj=io.BytesIO(inputs[key]), mode='r|*') as archive:
            for member in archive:
                count += 1
                expanded += max(0, member.size)
                if count > 100000 or expanded > MAX_ARCHIVE_EXPANDED:
                    raise EvidenceError('source archive exceeds inspection bound')
                if member.name in wanted:
                    if member.name in seen or not member.isfile():
                        raise EvidenceError('notice is duplicated or not a regular archive member')
                    seen.add(member.name)
                    row = wanted[member.name]
                    if member.size != row['bytes']:
                        raise EvidenceError('archive notice size differs')
                    with archive.extractfile(member) as stream:
                        data = stream.read(MAX_NOTICE + 1)
                    check_bytes(data, row)
                    notices[member.name] = data
        if seen != wanted.keys():
            raise EvidenceError('required archive notice is missing')
    for row in lock['supplementalLicenses']:
        data = regular(directory / row['name'], MAX_NOTICE)
        check_bytes(data, row)
        notices[row['name']] = data
    if sum(map(len, notices.values())) > MAX_NOTICE:
        raise EvidenceError('combined notice text exceeds bound')
    return notices


def json_bytes(value) -> bytes:
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


def source_bundle(lock: dict, inputs: dict, local: dict | None = None) -> tuple[bytes, list[dict]]:
    rows, modes = source_records(lock)
    entries = {r.get('archivePath', f"{r['origin']}/{r.get('name', '')}"):
               local[r['path']] if 'path' in r else inputs[(r['origin'], r['name'])] for r in rows}
    for name, data in entries.items():
        if modes.get(name) == '120000':
            target = plain_name(data.decode('utf-8'))
            sibling = name.rsplit('/', 1)[0] + '/' + target
            if sibling not in entries or modes.get(sibling) not in ('100644', '100755'):
                raise EvidenceError('recipe symlink does not name an included regular sibling')
    entries['SOURCE-MANIFEST.json'] = json_bytes(rows)
    entries['README.txt'] = (
        'Covalent Alpine runtime corresponding source\n\n'
        'Each origin directory contains the exact Alpine APKBUILD, patches, configuration,\n'
        'install/trigger scripts and its declared upstream source inputs. Archives remain\n'
        'in their original form. SOURCE-MANIFEST.json records upstream URLs and hashes.\n'
        'Use the Alpine abuild environment for Alpine 3.23 and the recorded architecture\n'
        'to build a package from its APKBUILD. Build dependencies are declared there.\n'
        'Covalent does not modify these source files. Aports commits are in inventory.json.\n'
        'Source retains its original component licenses and copyright notices; Covalent\n'
        'does not impose additional restrictions on these components. Full license texts\n'
        'and component references are in the adjacent THIRD-PARTY-NOTICES.txt.\n'
    ).encode()
    if lock['schema'] == 2:
        entries['README.txt'] = entries['README.txt'].replace(
            b'Covalent does not modify these source files. Aports commits are in inventory.json.\n',
            b'Local changes are explicit in inventory.json localRecipeOverrides. For BusyBox,\n'
            b'aport/ holds the complete modified recipe tree; its original APKBUILD, exact\n'
            b'delta and provenance are beside it. The upstream tarball is included once.\n'
            b'Copy that tarball to the abuild source cache before building the local aport.\n'
            b'The base aports commit is provenance, not a commit embedded in local packages.\n')
    buffer = io.BytesIO()
    with gzip.GzipFile(fileobj=buffer, mode='wb', filename='', mtime=0, compresslevel=9) as gz:
        with tarfile.open(fileobj=gz, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name, data in sorted(entries.items()):
                member = tarfile.TarInfo(name)
                member.size = len(data)
                member.mode = 0o755 if modes.get(name) == '100755' else 0o644
                member.mtime = 0
                if modes.get(name) == '120000':
                    member.type = tarfile.SYMTYPE
                    member.linkname = data.decode('utf-8')
                    member.mode = 0o777
                    member.size = 0
                    archive.addfile(member)
                else:
                    archive.addfile(member, io.BytesIO(data))
    result = buffer.getvalue()
    if len(result) > MAX_SOURCE:
        raise EvidenceError('packaged source exceeds image evidence budget')
    return result, rows


def publish_output(output: Path, files: dict[str, bytes]) -> None:
    """Create a new private output through held descriptors; never replace existing output.

    Consumers must wait for this command to succeed. On failure, remove only the
    files created here. The build workspace must not be shared with other writers.
    """
    output = output.absolute()
    plain_name(output.name)
    parent = directory_fd(output.parent)
    child = -1
    created = []
    complete = False
    identity = lambda s: (s.st_dev, s.st_ino)
    try:
        try:
            os.mkdir(output.name, 0o700, dir_fd=parent)
        except FileExistsError as error:
            raise EvidenceError('output must be new under a canonical existing directory') from error
        child = os.open(output.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
        for name, data in files.items():
            plain_name(name)
            fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=child)
            created.append(name)
            try:
                pending = memoryview(data)
                while pending:
                    count = os.write(fd, pending)
                    if count <= 0:
                        raise EvidenceError('output write did not complete')
                    pending = pending[count:]
                os.fchmod(fd, 0o444)
                os.fsync(fd)
            finally:
                os.close(fd)
        os.fchmod(child, 0o755)
        os.fsync(child)
        current_parent = directory_fd(output.parent)
        try:
            if (identity(os.fstat(parent)) != identity(os.fstat(current_parent))
                    or identity(os.fstat(child)) != identity(os.stat(output.name, dir_fd=parent, follow_symlinks=False))):
                raise EvidenceError('output directory changed during collection')
        finally:
            os.close(current_parent)
        complete = True
    finally:
        if child >= 0:
            if not complete:
                for name in created:
                    os.unlink(name, dir_fd=child)
                try:
                    current = os.stat(output.name, dir_fd=parent, follow_symlinks=False)
                    if identity(current) == identity(os.fstat(child)):
                        os.rmdir(output.name, dir_fd=parent)
                except FileNotFoundError:
                    pass
            os.close(child)
        os.close(parent)


def collect(args) -> dict:
    lock, lock_data = load_lock(args.lock)
    installed_data = regular(args.installed, 4 * 1024 * 1024)
    actual = parse_database(installed_data, missing_commit_packages(lock))
    expected = expected_packages(lock, args.arch)
    if actual != expected:
        raise EvidenceError('installed runtime packages differ from reviewed lock')
    ca = lock['copiedCABundle']
    ca_data = regular(args.ca_bundle, MAX_NOTICE)
    check_bytes(ca_data, ca)
    builder_data = regular(args.builder_installed, 4 * 1024 * 1024)
    builder = parse_database(builder_data)
    ca_record = [r for r in builder if r['name'] == ca['package']]
    expected_ca = [r for r in expected if r['name'] == ca['package']]
    declared_ca = {k: ca['package'] if k == 'name' else ca['aportsCommit'] if k == 'packageCommit' else ca[k]
                   for k in PACKAGE_FIELDS.values() if k != 'architecture'}
    if not ca_record or ca_record != expected_ca or {k: v for k, v in ca_record[0].items() if k != 'architecture'} != declared_ca:
        raise EvidenceError('copied CA builder package provenance differs')
    inputs = collect_inputs(lock, args.cache, args.download)
    local = collect_local_inputs(lock, inputs, getattr(args, 'local_sources', None))
    notices = read_notices(lock, inputs, args.licenses)
    archive, packaged = source_bundle(lock, inputs, local)
    lines = ['Covalent Alpine runtime notices', '',
             'This inventory covers installed Alpine packages and the CA file copied from the',
             'pinned Go builder. Separate Caddy and Syncthing notices cover those binaries.',
             'Package license expressions come from the exact Alpine installed database.',
             'Full corresponding source for every GPL/MPL origin is in runtime-source.tar.gz.',
             'The archive also includes small permissive components. OpenSSL source remains',
             'available at its hash-locked upstream URL in inventory.json. OpenSSL AUTHORS',
             'and Apache license text are included below. No source recipe was executed.',
             'CA source includes build scripts; their curl/MIT notices are retained even though',
             'those scripts are not runtime executables. Copyright remains with each author.', '']
    for row in actual:
        origin = row['origin']
        base = next(r['aportsCommit'] for r in lock['packages'] if r['name'] == row['name'])
        provenance = (f"aports: {row['packageCommit']}" if row['packageCommit'] is not None else
                      f"package commit: absent; base recipe: Alpine aports {base} (Covalent-local modification)")
        lines += [f"{row['name']} {row['version']} ({row['architecture']})",
                  f"License: {row['license']}; origin: {origin}; {provenance}",
                  'Notices: ' + ', '.join(lock['originNoticeReferences'][origin]),
                  'Source: ' + (f'runtime-source.tar.gz/{origin}/' if origin in lock['sourceBundleOrigins']
                                else 'hash-locked upstream source in inventory.json'), '']
    for name, data in sorted(notices.items()):
        lines += ['=' * 72, name, '=' * 72, data.decode('utf-8'), '']
    notice_data = '\n'.join(lines).encode()
    if len(notice_data) > MAX_NOTICE:
        raise EvidenceError('readable notice bundle exceeds bound')
    inventory = {
        'schema': lock['schema'], 'target': 'linux-' + args.arch, 'lockSHA256': sha(lock_data),
        'declaredSourceImages': {'baseIndexDigest': lock['baseIndexDigest'],
                                 'golangIndexDigest': lock['golangIndexDigest'],
                                 'scope': 'Reviewed Dockerfile source pins; final OCI image identity is bound by the separate container contract.'},
        'installedDatabaseSHA256': sha(installed_data), 'packages': actual,
        'copiedCABundle': {**ca, 'builderInstalledDatabaseSHA256': sha(builder_data),
                           'builderPackage': ca_record[0]},
        'sourceInputs': lock['files'], 'sourceBundleOrigins': lock['sourceBundleOrigins'],
        'recipeDirectories': lock['recipeDirectories'],
        'sourceBundle': {'path': 'runtime-source.tar.gz', 'bytes': len(archive), 'sha256': sha(archive),
                         'sourceFiles': len(packaged)},
        'noticeBundle': {'path': 'THIRD-PARTY-NOTICES.txt', 'bytes': len(notice_data), 'sha256': sha(notice_data)},
        'archiveNotices': lock['archiveNotices'], 'supplementalLicenses': lock['supplementalLicenses'],
        'originNoticeReferences': lock['originNoticeReferences'], 'unclassifiedOrigins': [],
        'sourceScope': 'Exact locked inputs and complete recorded recipe directories; recipes were not executed.',
    }
    if lock['schema'] == 2:
        inventory['localRecipeOverrides'] = lock['localRecipeOverrides']
    inventory_data = json_bytes(inventory)
    if len(inventory_data) > MAX_INVENTORY:
        raise EvidenceError('inventory exceeds bound')
    publish_output(args.output, {'runtime-source.tar.gz': archive,
                                'THIRD-PARTY-NOTICES.txt': notice_data, 'inventory.json': inventory_data})
    for row in actual:
        print('ALPINE_PACKAGE ' + json.dumps(row, sort_keys=True, separators=(',', ':')))
    receipt = {'target': inventory['target'], 'packages': len(actual),
               'origins': len(lock['originNoticeReferences']), 'unclassifiedOrigins': 0,
               'sourceBundle': inventory['sourceBundle'], 'noticeBundle': inventory['noticeBundle'],
               'inventoryBytes': len(inventory_data), 'inventorySHA256': sha(inventory_data),
               'caSHA256': sha(ca_data)}
    print('ALPINE_EVIDENCE ' + json.dumps(receipt, sort_keys=True, separators=(',', ':')))
    return inventory


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('lock', 'installed', 'builder-installed', 'ca-bundle', 'licenses', 'cache', 'output'):
        parser.add_argument('--' + option, type=Path, required=True)
    parser.add_argument('--arch', choices=('amd64', 'arm64'), required=True)
    parser.add_argument('--local-sources', type=Path, help='Reviewed local recipe overlay directory')
    parser.add_argument('--download', action='store_true', help='Fetch missing inputs only from hash-locked HTTPS URLs')
    args = parser.parse_args()
    try:
        collect(args)
    except (EvidenceError, OSError, ValueError, KeyError, TypeError, tarfile.TarError) as error:
        message = str(error) if isinstance(error, EvidenceError) else 'invalid or unavailable runtime evidence'
        print('Alpine runtime evidence failed: ' + message, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
