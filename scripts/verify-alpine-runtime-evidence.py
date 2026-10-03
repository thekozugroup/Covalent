#!/usr/bin/env python3
"""Verify extracted Alpine distribution files against the final image and reviewed locks."""
from __future__ import annotations

import argparse
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import zlib

SPEC = importlib.util.spec_from_file_location('alpine_collector', Path(__file__).with_name('collect-alpine-runtime-evidence.py'))
C = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(C)
OUTPUT_NAMES = {'inventory.json', 'THIRD-PARTY-NOTICES.txt', 'runtime-source.tar.gz'}
MAX_TAR = C.MAX_INPUTS + 1024 * 1024


def require(condition: bool, message: str) -> None:
    if not condition:
        raise C.EvidenceError(message)


def parse_json(data: bytes) -> dict:
    value = json.loads(data, object_pairs_hook=C.unique_object)
    require(isinstance(value, dict), 'evidence JSON must be an object')
    return value


def verify_source(data: bytes, lock: dict, reviewed: dict) -> None:
    decoder = zlib.decompressobj(wbits=31)
    raw = decoder.decompress(data, MAX_TAR + 1)
    require(len(raw) <= MAX_TAR, 'source archive exceeds expanded bound')
    require(decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail,
            'source archive must contain one complete gzip stream without trailing data')
    rows, modes = C.source_records(lock)
    expected = {r.get('archivePath', f"{r['origin']}/{r.get('name', '')}"): r for r in rows}
    manifest = C.json_bytes(rows)
    require(reviewed['sourceManifestSHA256'] == C.sha(manifest)
            and type(reviewed['sourceFiles']) is int and reviewed['sourceFiles'] == len(rows),
            'reviewed source manifest differs from locked inputs')
    names = set(expected) | {'SOURCE-MANIFEST.json', 'README.txt'}
    seen = set()
    members = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        for member in archive:
            name = C.relative_name(member.name)
            require(name in names and name not in seen, 'source member is duplicated or unexpected')
            seen.add(name)
            require(len(seen) <= 514, 'source member count exceeds bound')
            symlink = modes.get(name) == '120000'
            mode = 0o777 if symlink else (0o755 if modes.get(name) == '100755' else 0o644)
            require(member.mode == mode and member.uid == 0 and member.gid == 0
                    and member.uname == '' and member.gname == '' and member.mtime == 0
                    and member.pax_headers == {}, 'source member metadata differs')
            if symlink:
                require(member.issym() and member.size == 0, 'source recipe link type differs')
                payload = member.linkname.encode('utf-8')
                sibling = name.rsplit('/', 1)[0] + '/' + C.plain_name(member.linkname)
                require(sibling in expected and modes.get(sibling) in ('100644', '100755'),
                        'source recipe link target differs')
            else:
                require(member.isfile() and member.linkname == '', 'source member must be regular')
                limit = expected[name]['bytes'] if name in expected else C.MAX_LOCK
                require(0 <= member.size <= limit, 'source member exceeds bound')
                with archive.extractfile(member) as stream:
                    payload = stream.read(limit + 1)
                require(len(payload) == member.size, 'source member is incomplete')
            if name in expected:
                C.check_bytes(payload, expected[name])
            elif name == 'SOURCE-MANIFEST.json':
                require(payload == manifest, 'packaged source manifest differs')
            else:
                require(C.sha(payload) == reviewed['sourceReadmeSHA256'], 'source build instructions differ')
            members[name] = (member, payload)
    require(seen == names, 'source bundle is incomplete')
    # Rebuild the reviewed USTAR encoding to reject hidden trailing data, duplicate
    # gzip streams, alternate header records and nonzero padding after the tar EOF.
    canonical = io.BytesIO()
    with tarfile.open(fileobj=canonical, mode='w', format=tarfile.USTAR_FORMAT) as archive:
        for name, (member, payload) in sorted(members.items()):
            clean = tarfile.TarInfo(name)
            clean.mode = member.mode
            clean.size = len(payload)
            if member.issym():
                clean.type = tarfile.SYMTYPE
                clean.linkname = member.linkname
                clean.size = 0
                archive.addfile(clean)
            else:
                archive.addfile(clean, io.BytesIO(payload))
    require(raw == canonical.getvalue(), 'source tar encoding differs from canonical reviewed members')


def verify(args) -> dict:
    lock, lock_data = C.load_lock(args.lock)
    packaged = parse_json(C.regular(args.packaged_lock, C.MAX_LOCK))
    require(packaged.get('schema') == 1 and packaged.get('sourceLockSHA256') == C.sha(lock_data)
            and set(packaged.get('targets', {})) == {'amd64', 'arm64'}, 'packaged evidence lock differs')
    reviewed = packaged['targets'][args.arch]
    require(set(reviewed) == {'noticeBundle', 'sourceReadmeSHA256', 'sourceManifestSHA256',
                              'sourceFiles', 'builderInstalledDatabaseSHA256'}, 'reviewed target fields differ')
    require(all(isinstance(reviewed[k], str) and C.HEX.fullmatch(reviewed[k]) for k in
                ('sourceReadmeSHA256', 'sourceManifestSHA256', 'builderInstalledDatabaseSHA256')),
            'reviewed target digest is malformed')
    root = C.directory_fd(args.bundle)
    try:
        require(set(os.listdir(root)) == OUTPUT_NAMES, 'packaged evidence file set differs')
        files = {}
        for name, limit in [('inventory.json', C.MAX_INVENTORY),
                            ('THIRD-PARTY-NOTICES.txt', C.MAX_NOTICE), ('runtime-source.tar.gz', C.MAX_SOURCE)]:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=root)
            try:
                files[name] = C.read_fd(fd, limit)
            finally:
                os.close(fd)
    finally:
        os.close(root)
    inventory = parse_json(files['inventory.json'])
    required_fields = {'schema', 'target', 'lockSHA256', 'declaredSourceImages', 'installedDatabaseSHA256',
                       'packages', 'copiedCABundle', 'sourceInputs', 'sourceBundleOrigins', 'recipeDirectories',
                       'sourceBundle', 'noticeBundle', 'archiveNotices', 'supplementalLicenses',
                       'originNoticeReferences', 'unclassifiedOrigins', 'sourceScope'}
    if lock['schema'] == 2:
        required_fields.add('localRecipeOverrides')
        require(inventory.get('localRecipeOverrides') == lock['localRecipeOverrides'], 'local recipe provenance differs')
    require(set(inventory) == required_fields and inventory['schema'] == lock['schema']
            and inventory['target'] == 'linux-' + args.arch and inventory['lockSHA256'] == C.sha(lock_data),
            'inventory schema or target differs')
    installed = C.regular(args.installed, C.MAX_INVENTORY)
    actual = C.parse_database(installed, C.missing_commit_packages(lock))
    expected = C.expected_packages(lock, args.arch)
    require(actual == expected and inventory['packages'] == actual
            and inventory['installedDatabaseSHA256'] == C.sha(installed),
            'final image package database differs')
    ca = lock['copiedCABundle']
    C.check_bytes(C.regular(args.ca_bundle, C.MAX_NOTICE), ca)
    ca_packages = [r for r in expected if r['name'] == ca['package']]
    require(len(ca_packages) == 1, 'copied CA package is missing')
    expected_ca = {**ca, 'builderInstalledDatabaseSHA256': reviewed['builderInstalledDatabaseSHA256'],
                   'builderPackage': ca_packages[0]}
    require(inventory['copiedCABundle'] == expected_ca, 'copied CA provenance differs')
    require(inventory['declaredSourceImages'] == {
        'baseIndexDigest': lock['baseIndexDigest'], 'golangIndexDigest': lock['golangIndexDigest'],
        'scope': 'Reviewed Dockerfile source pins; final OCI image identity is bound by the separate container contract.'},
        'declared image provenance differs')
    for key in ('sourceBundleOrigins', 'recipeDirectories', 'archiveNotices',
                'supplementalLicenses', 'originNoticeReferences'):
        require(inventory[key] == lock[key], 'packaged classification differs from source lock')
    require(inventory['sourceInputs'] == lock['files'] and inventory['unclassifiedOrigins'] == []
            and inventory['sourceScope'] == 'Exact locked inputs and complete recorded recipe directories; recipes were not executed.',
            'packaged source scope differs')
    for field, name in [('noticeBundle', 'THIRD-PARTY-NOTICES.txt'), ('sourceBundle', 'runtime-source.tar.gz')]:
        record = inventory[field]
        expected_record = {'path': name, 'bytes': len(files[name]), 'sha256': C.sha(files[name])}
        if field == 'sourceBundle':
            expected_record['sourceFiles'] = reviewed['sourceFiles']
        require(record == expected_record, 'packaged file digest or metadata differs')
    require(inventory['noticeBundle'] == reviewed['noticeBundle'], 'readable notices differ from reviewed texts')
    verify_source(files['runtime-source.tar.gz'], lock, reviewed)
    report = {'status': 'passed', 'target': inventory['target'], 'packages': len(actual),
              'sourceFiles': reviewed['sourceFiles'], 'inventorySHA256': C.sha(files['inventory.json']),
              'noticeSHA256': C.sha(files['THIRD-PARTY-NOTICES.txt']),
              'sourceSHA256': C.sha(files['runtime-source.tar.gz']), 'caSHA256': ca['sha256']}
    print('ALPINE_PACKAGED_EVIDENCE ' + json.dumps(report, sort_keys=True, separators=(',', ':')))
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('bundle', 'installed', 'ca-bundle', 'lock', 'packaged-lock'):
        parser.add_argument('--' + option, type=Path, required=True)
    parser.add_argument('--arch', choices=('amd64', 'arm64'), required=True)
    try:
        verify(parser.parse_args())
    except (C.EvidenceError, OSError, ValueError, KeyError, TypeError, tarfile.TarError, EOFError, zlib.error) as error:
        message = str(error) if isinstance(error, C.EvidenceError) else 'invalid or unavailable packaged evidence'
        print('Alpine packaged evidence failed: ' + message, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
