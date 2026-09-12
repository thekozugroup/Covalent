#!/usr/bin/env python3
"""Reject altered packaged evidence even when its own inventory is self-consistent."""
import contextlib
import gzip
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
from types import SimpleNamespace
import unittest


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


V = module('alpine_verifier', 'verify-alpine-runtime-evidence.py')
F = module('alpine_fixtures', 'test-collect-alpine-runtime-evidence.py')


class PackagedTests(unittest.TestCase):
    def setUp(self):
        self.prepare()

    def prepare(self, local=False):
        self.fixture = F.EvidenceTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        if local:
            self.fixture.add_local_busybox()
        self.root = self.fixture.root
        self.inventory = self.fixture.run_collector()
        self.bundle = self.fixture.args.output
        with tarfile.open(self.bundle / 'runtime-source.tar.gz') as archive:
            readme = archive.extractfile('README.txt').read()
            manifest = archive.extractfile('SOURCE-MANIFEST.json').read()
        record = {'noticeBundle': self.inventory['noticeBundle'], 'sourceReadmeSHA256': V.C.sha(readme),
                  'sourceManifestSHA256': V.C.sha(manifest), 'sourceFiles': self.inventory['sourceBundle']['sourceFiles'],
                  'builderInstalledDatabaseSHA256': self.inventory['copiedCABundle']['builderInstalledDatabaseSHA256']}
        self.packaged = {'schema': 1, 'sourceLockSHA256': self.inventory['lockSHA256'],
                         'targets': {'amd64': record, 'arm64': record}}
        self.args = SimpleNamespace(bundle=self.bundle, installed=self.fixture.args.installed,
                                    ca_bundle=self.fixture.args.ca_bundle, lock=self.fixture.args.lock,
                                    packaged_lock=self.root / 'packaged-lock.json', arch='amd64')
        self.args.packaged_lock.write_bytes(V.C.json_bytes(self.packaged))

    def verify(self):
        with contextlib.redirect_stdout(io.StringIO()):
            return V.verify(self.args)

    def write(self, name, data):
        path = self.bundle / name
        path.chmod(0o600)
        path.write_bytes(data)

    def save_inventory(self):
        self.write('inventory.json', V.C.json_bytes(self.inventory))

    def replace_archive(self, raw):
        data = gzip.compress(raw, compresslevel=1, mtime=123456)
        self.write('runtime-source.tar.gz', data)
        self.inventory['sourceBundle'].update(bytes=len(data), sha256=V.C.sha(data))
        self.save_inventory()

    def changed_tar(self, alter):
        buffer = io.BytesIO()
        with tarfile.open(self.bundle / 'runtime-source.tar.gz') as source:
            rows = [(m, source.extractfile(m).read()) for m in source]
        rows = alter(rows)
        with tarfile.open(fileobj=buffer, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for member, data in rows:
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        return buffer.getvalue()

    def test_exact_packaged_fixture_passes(self):
        self.assertEqual(self.verify()['sourceFiles'], 2)

    def test_local_package_database_and_provenance_are_bound(self):
        self.prepare(local=True)
        self.assertEqual(self.verify()['status'], 'passed')
        self.args.installed.write_bytes(self.fixture.database.replace(b'o:busybox\n', b'c:\no:busybox\n'))
        with self.assertRaisesRegex(V.C.EvidenceError, 'package database'):
            self.verify()
        self.args.installed.write_bytes(self.fixture.database)
        self.inventory['localRecipeOverrides'][0]['localRecipeTreeSHA1'] = 'f' * 40
        self.save_inventory()
        with self.assertRaisesRegex(V.C.EvidenceError, 'local recipe provenance'):
            self.verify()

    def test_equivalent_gzip_recompression_passes(self):
        old = (self.bundle / 'runtime-source.tar.gz').read_bytes()
        self.replace_archive(gzip.decompress(old))
        self.assertNotEqual(old, (self.bundle / 'runtime-source.tar.gz').read_bytes())
        self.assertEqual(self.verify()['status'], 'passed')

    def test_actual_image_package_and_ca_drift_fail(self):
        self.args.installed.write_bytes(self.fixture.database.replace(b'1.0-r0', b'1.1-r0'))
        with self.assertRaisesRegex(V.C.EvidenceError, 'package database'):
            self.verify()
        self.args.installed.write_bytes(self.fixture.database)
        self.args.ca_bundle.write_bytes(b'changed')
        with self.assertRaises(V.C.EvidenceError):
            self.verify()

    def test_self_consistent_notice_substitution_fails(self):
        data = b'Replacement notices\n'
        self.write('THIRD-PARTY-NOTICES.txt', data)
        self.inventory['noticeBundle'] = {'path': 'THIRD-PARTY-NOTICES.txt', 'bytes': len(data), 'sha256': V.C.sha(data)}
        self.save_inventory()
        with self.assertRaisesRegex(V.C.EvidenceError, 'reviewed texts'):
            self.verify()

    def test_self_consistent_source_substitution_fails(self):
        def alter(rows):
            member, data = rows[-1]
            rows[-1] = (member, data + b'changed')
            return rows
        self.replace_archive(self.changed_tar(alter))
        with self.assertRaises(V.C.EvidenceError):
            self.verify()

    def test_missing_duplicate_and_extra_source_members_fail(self):
        original = (self.bundle / 'runtime-source.tar.gz').read_bytes()
        for kind in ('missing', 'duplicate', 'extra'):
            self.write('runtime-source.tar.gz', original)
            def alter(rows):
                if kind == 'missing':
                    return rows[:-1]
                if kind == 'duplicate':
                    return rows + [rows[-1]]
                return rows + [(tarfile.TarInfo('extra'), b'extra')]
            self.replace_archive(self.changed_tar(alter))
            with self.subTest(kind=kind), self.assertRaises(V.C.EvidenceError):
                self.verify()

    def test_wrong_archive_metadata_and_hidden_trailing_bytes_fail(self):
        original = (self.bundle / 'runtime-source.tar.gz').read_bytes()
        def alter(rows):
            rows[0][0].uid = 1000
            return rows
        self.replace_archive(self.changed_tar(alter))
        with self.assertRaisesRegex(V.C.EvidenceError, 'metadata'):
            self.verify()
        self.replace_archive(gzip.decompress(original) + b'hidden')
        with self.assertRaisesRegex(V.C.EvidenceError, 'canonical'):
            self.verify()

    def test_packaged_file_set_symlinks_and_fifos_fail(self):
        extra = self.bundle / 'extra'
        extra.write_text('extra')
        with self.assertRaisesRegex(V.C.EvidenceError, 'file set'):
            self.verify()
        extra.unlink()
        path = self.bundle / 'inventory.json'
        path.unlink()
        path.symlink_to(self.args.lock)
        with self.assertRaises((V.C.EvidenceError, OSError)):
            self.verify()
        path.unlink()
        os.mkfifo(path)
        with self.assertRaises(V.C.EvidenceError):
            self.verify()

    def test_gzip_trailers_and_additional_streams_fail(self):
        original = (self.bundle / 'runtime-source.tar.gz').read_bytes()
        for trailer in (b'unreviewed', gzip.compress(b''), gzip.compress(b'extra')):
            data = original + trailer
            self.write('runtime-source.tar.gz', data)
            self.inventory['sourceBundle'].update(bytes=len(data), sha256=V.C.sha(data))
            self.save_inventory()
            with self.subTest(trailer=trailer), self.assertRaisesRegex(V.C.EvidenceError, 'one complete gzip'):
                self.verify()

    def test_reviewed_lock_and_builder_provenance_cannot_drift(self):
        self.inventory['copiedCABundle']['builderInstalledDatabaseSHA256'] = '0' * 64
        self.save_inventory()
        with self.assertRaisesRegex(V.C.EvidenceError, 'CA provenance'):
            self.verify()
        self.packaged['sourceLockSHA256'] = '0' * 64
        self.args.packaged_lock.write_bytes(V.C.json_bytes(self.packaged))
        with self.assertRaisesRegex(V.C.EvidenceError, 'lock differs'):
            self.verify()

    def test_duplicate_json_keys_are_rejected(self):
        self.write('inventory.json', b'{"schema":1,"schema":1}')
        with self.assertRaisesRegex(V.C.EvidenceError, 'duplicate JSON'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
