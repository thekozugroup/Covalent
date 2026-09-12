#!/usr/bin/env python3
"""Adversarial offline tests for the Alpine distribution evidence collector."""
import contextlib
import difflib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location('alpine_evidence', Path(__file__).with_name('collect-alpine-runtime-evidence.py'))
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)


def archive_bytes(kind='regular'):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w:gz') as archive:
        member = tarfile.TarInfo('source/LICENSE')
        data = b'This is a synthetic license fixture, not an upstream license.\n'
        if kind == 'symlink':
            member.type = tarfile.SYMTYPE
            member.linkname = '../../outside'
            archive.addfile(member)
        else:
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
            if kind == 'duplicate':
                archive.addfile(member, io.BytesIO(data))
    return buffer.getvalue(), data


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='covalent-alpine-evidence-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cache = self.root / 'cache'
        self.origin = 'ca-certificates'
        (self.cache / self.origin).mkdir(parents=True)
        self.archive, self.license = archive_bytes()
        checksum = hashlib.sha512(self.archive).hexdigest()
        self.recipe = f'pkgver=1.0\npkgrel=0\nsha512sums="\n{checksum}  fixture.tar.gz\n"\n'.encode()
        files = []
        for name, data in [('APKBUILD', self.recipe), ('fixture.tar.gz', self.archive)]:
            (self.cache / self.origin / name).write_bytes(data)
            row = {'origin': self.origin, 'name': name, 'bytes': len(data), 'sha256': M.sha(data)}
            if name == 'APKBUILD':
                row['gitBlob'] = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
                row['url'] = 'https://raw.githubusercontent.com/alpinelinux/aports/' + 'a' * 40 + '/main/ca-certificates/APKBUILD'
            else:
                row.update(url='https://distfiles.alpinelinux.org/distfiles/v3.23/fixture.tar.gz', apkbuildSHA512=checksum)
            files.append(row)
        package = {'name': self.origin + '-bundle', 'origin': self.origin, 'version': '1.0-r0',
                   'license': 'MPL-2.0', 'aportsCommit': 'a' * 40}
        self.ca = b'Synthetic CA data\n'
        self.lock = {'schema': 1, 'architectures': {'amd64': 'x86_64', 'arm64': 'aarch64'},
                     'limits': dict(M.LOCK_LIMITS),
                     'baseIndexDigest': 'sha256:' + 'b' * 64, 'golangIndexDigest': 'sha256:' + 'c' * 64,
                     'packages': [package], 'sourceBundleOrigins': [self.origin], 'files': files,
                     'copiedCABundle': {k: v for k, v in package.items() if k != 'name'},
                     'archiveNotices': [{'origin': self.origin, 'archive': 'fixture.tar.gz', 'member': 'source/LICENSE',
                                         'bytes': len(self.license), 'sha256': M.sha(self.license)}],
                     'supplementalLicenses': [], 'originNoticeReferences': {self.origin: ['source/LICENSE']}}
        tree_entries = [{'name': 'APKBUILD', 'mode': '100644', 'gitBlob': files[0]['gitBlob']}]
        self.lock['recipeDirectories'] = [{'origin': self.origin, 'aportsCommit': 'a' * 40,
                                           'entries': tree_entries, 'treeSHA1': M.git_tree_digest(tree_entries)}]
        self.lock['copiedCABundle'].update(package=package['name'], bytes=len(self.ca), sha256=M.sha(self.ca))
        self.database = '\n'.join(k + ':' + (package['aportsCommit'] if v == 'packageCommit' else package[v] if v != 'architecture' else 'x86_64')
                                  for k, v in M.PACKAGE_FIELDS.items()).encode() + b'\n'
        for name, data in [('installed', self.database), ('builder', self.database), ('ca', self.ca)]:
            (self.root / name).write_bytes(data)
        (self.root / 'licenses').mkdir()
        self.args = SimpleNamespace(lock=self.root / 'lock.json', installed=self.root / 'installed',
                                    builder_installed=self.root / 'builder', ca_bundle=self.root / 'ca',
                                    cache=self.cache, licenses=self.root / 'licenses', arch='amd64',
                                    output=self.root / 'output', download=False)
        self.save_lock()

    def save_lock(self):
        self.args.lock.write_text(json.dumps(self.lock))

    def run_collector(self):
        with contextlib.redirect_stdout(io.StringIO()):
            return M.collect(self.args)

    def add_local_busybox(self):
        """Small synthetic local recipe, alongside an unchanged CA package."""
        self.lock['schema'] = 2
        origin = 'busybox'
        (self.cache / origin).mkdir()
        files = []
        for old in list(self.lock['files']):
            row = {**old, 'origin': origin, 'url': old['url'].replace('/ca-certificates/', '/busybox/')}
            files.append(row)
            (self.cache / origin / row['name']).write_bytes((self.cache / self.origin / row['name']).read_bytes())
        self.lock['files'].extend(files)
        self.lock['sourceBundleOrigins'].append(origin)
        self.lock['originNoticeReferences'][origin] = ['source/LICENSE']
        entries = [{'name': 'APKBUILD', 'mode': '100644', 'gitBlob': files[0]['gitBlob']}]
        base_tree = M.git_tree_digest(entries)
        self.lock['recipeDirectories'].append({'origin': origin, 'aportsCommit': 'a' * 40,
                                               'entries': entries, 'treeSHA1': base_tree})
        self.args.local_sources = self.root / 'local'
        (self.args.local_sources / 'aport').mkdir(parents=True)
        patch = b'Synthetic local patch fixture.\n'
        checksum = hashlib.sha512(patch).hexdigest()
        recipe = self.recipe.replace(b'pkgrel=0', b'pkgrel=1000').replace(b'\n"\n', f'\n{checksum}  fix.patch\n"\n'.encode())
        base_name = 'APKBUILD.alpine-1.0-r0'
        delta = ''.join(difflib.unified_diff(self.recipe.decode().splitlines(True), recipe.decode().splitlines(True),
                                            fromfile=base_name, tofile='aport/APKBUILD')).encode()
        def local_record(path, data, git=False, apk=False):
            (self.args.local_sources / path).write_bytes(data)
            row = {'path': path, 'bytes': len(data), 'sha256': M.sha(data)}
            if git:
                row['gitBlob'] = M.git_blob(data)
            if apk:
                row['apkbuildSHA512'] = hashlib.sha512(data).hexdigest()
            return row
        local_recipe = local_record('aport/APKBUILD', recipe, git=True)
        local_patch = local_record('aport/fix.patch', patch, git=True, apk=True)
        changed = [{'name': 'APKBUILD', 'mode': '100644', 'gitBlob': local_recipe['gitBlob']},
                   {'name': 'fix.patch', 'mode': '100644', 'gitBlob': local_patch['gitBlob']}]
        self.lock['localRecipeOverrides'] = [{'origin': origin, 'packageNames': ['busybox', 'busybox-binsh', 'ssl_client'],
            'baseVersion': '1.0-r0', 'localVersion': '1.0-r1000', 'baseAportsCommit': 'a' * 40,
            'baseRecipeTreeSHA1': base_tree, 'localRecipeTreeSHA1': M.git_tree_digest(changed),
            'baseAPKBUILDName': base_name, 'localAPKBUILD': local_recipe, 'addedRecipeFiles': [local_patch],
            'transformation': local_record('APKBUILD.covalent.patch', delta),
            'provenance': local_record('PROVENANCE.md', b'Synthetic local provenance fixture.\n')}]
        for name in ('busybox', 'busybox-binsh', 'ssl_client'):
            row = {'name': name, 'version': '1.0-r1000', 'origin': origin, 'license': 'GPL-2.0-only',
                   'aportsCommit': 'a' * 40, 'installedDatabaseCommit': None}
            if name == 'busybox-binsh':
                row['architecture'] = 'noarch'
            self.lock['packages'].append(row)
            self.database += ('\n' + '\n'.join([f'P:{name}', 'V:1.0-r1000',
                'A:' + row.get('architecture', 'x86_64'), 'o:busybox', 'L:GPL-2.0-only']) + '\n').encode()
        self.args.installed.write_bytes(self.database)
        self.save_lock()

    def test_local_source_records_absent_package_commit_and_complete_tree(self):
        self.add_local_busybox()
        result = self.run_collector()
        local = [r for r in result['packages'] if r['origin'] == 'busybox']
        self.assertEqual(len(local), 3)
        self.assertTrue(all(r['packageCommit'] is None for r in local))
        self.assertEqual(next(r for r in local if r['name'] == 'busybox-binsh')['architecture'], 'noarch')
        self.assertIn('package commit: absent; base recipe: Alpine aports',
                      (self.args.output / 'THIRD-PARTY-NOTICES.txt').read_text())
        with tarfile.open(self.args.output / 'runtime-source.tar.gz') as archive:
            self.assertEqual(archive.extractfile('busybox/APKBUILD.alpine-1.0-r0').read(), self.recipe)
            for name in ('APKBUILD', 'fix.patch'):
                self.assertEqual(archive.extractfile('busybox/aport/' + name).read(),
                                 (self.args.local_sources / 'aport' / name).read_bytes())

    def test_local_commit_exceptions_do_not_admit_empty_or_upstream_fields(self):
        self.add_local_busybox()
        changes = [self.database.replace(b'o:busybox\n', b'c:\no:busybox\n'),
                   self.database.replace(b'A:noarch', b'A:x86_64'),
                   self.database.replace(b'c:' + b'a' * 40 + b'\n', b'')]
        for data in changes:
            self.args.installed.write_bytes(data)
            with self.assertRaises(M.EvidenceError):
                self.run_collector()

    def test_local_delta_and_tree_must_match_reviewed_base_and_result(self):
        self.add_local_busybox()
        override = self.lock['localRecipeOverrides'][0]
        original = override['localRecipeTreeSHA1']
        override['localRecipeTreeSHA1'] = 'f' * 40
        self.save_lock()
        with self.assertRaisesRegex(M.EvidenceError, 'local recipe tree'):
            self.run_collector()
        override['localRecipeTreeSHA1'] = original
        delta = b'Fake transformation with a matching declared digest.\n'
        (self.args.local_sources / 'APKBUILD.covalent.patch').write_bytes(delta)
        override['transformation'].update(bytes=len(delta), sha256=M.sha(delta))
        self.save_lock()
        with self.assertRaisesRegex(M.EvidenceError, 'local recipe transformation'):
            self.run_collector()

    def test_complete_output_is_deterministic_and_contains_exact_source(self):
        first = self.run_collector()
        self.args.output = self.root / 'second'
        self.assertEqual(first, self.run_collector())
        for name in ('inventory.json', 'THIRD-PARTY-NOTICES.txt', 'runtime-source.tar.gz'):
            self.assertEqual((self.root / 'output' / name).read_bytes(), (self.root / 'second' / name).read_bytes())
        with tarfile.open(self.root / 'output/runtime-source.tar.gz') as archive:
            self.assertEqual(archive.extractfile('ca-certificates/APKBUILD').read(), self.recipe)
            self.assertEqual(archive.extractfile('ca-certificates/fixture.tar.gz').read(), self.archive)
        self.assertEqual(first['unclassifiedOrigins'], [])

    def test_installed_version_architecture_unknown_and_duplicate_are_rejected(self):
        changes = [self.database.replace(b'1.0-r0', b'1.1-r0'), self.database.replace(b'x86_64', b'aarch64'),
                   self.database + b'\n' + self.database.replace(b'P:ca-certificates-bundle', b'P:extra'),
                   self.database + b'\n' + self.database]
        for data in changes:
            with self.subTest(data=data):
                self.args.installed.write_bytes(data)
                with self.assertRaises(M.EvidenceError):
                    self.run_collector()
                self.assertFalse(self.args.output.exists())

    def test_missing_and_repeated_identity_fields_are_rejected(self):
        for data in [self.database.replace(b'A:x86_64\n', b''), self.database + b'V:1.0-r0\n']:
            with self.assertRaises(M.EvidenceError):
                M.parse_database(data)

    def test_copied_ca_content_and_builder_provenance_are_checked(self):
        self.args.ca_bundle.write_bytes(b'changed CA')
        with self.assertRaises(M.EvidenceError):
            self.run_collector()
        self.args.ca_bundle.write_bytes(self.ca)
        self.args.builder_installed.write_bytes(self.database.replace(b'c:' + b'a' * 40, b'c:' + b'd' * 40))
        with self.assertRaises(M.EvidenceError):
            self.run_collector()

    def test_input_tampering_does_not_produce_output(self):
        (self.cache / self.origin / 'APKBUILD').write_bytes(self.recipe + b'# altered\n')
        with self.assertRaises(M.EvidenceError):
            self.run_collector()
        self.assertFalse(self.args.output.exists())

    def test_copyleft_source_and_notice_classification_cannot_be_omitted(self):
        self.lock['sourceBundleOrigins'] = []
        self.save_lock()
        with self.assertRaises(M.EvidenceError):
            M.load_lock(self.args.lock)
        self.lock['sourceBundleOrigins'] = [self.origin]
        self.lock['originNoticeReferences'] = {}
        self.save_lock()
        with self.assertRaises(M.EvidenceError):
            M.load_lock(self.args.lock)

    def test_source_paths_and_urls_are_restricted(self):
        for name in ('../escape', '/absolute', 'a/b', '..', 'a\nb'):
            with self.subTest(name=name), self.assertRaises(M.EvidenceError):
                M.plain_name(name)
        self.lock['files'][0]['url'] = 'https://example.com/source'
        self.save_lock()
        with self.assertRaises(M.EvidenceError):
            M.load_lock(self.args.lock)

    def test_duplicate_json_keys_and_source_records_are_rejected(self):
        self.args.lock.write_text('{"schema":1,"schema":1}')
        with self.assertRaises(M.EvidenceError):
            M.load_lock(self.args.lock)
        self.lock['files'].append(self.lock['files'][0])
        self.save_lock()
        with self.assertRaises(M.EvidenceError):
            M.load_lock(self.args.lock)

    def test_apkbuild_checksum_coverage_is_not_inferred_from_remaining_inputs(self):
        inputs = {(self.origin, 'APKBUILD'): self.recipe}
        self.lock['files'] = [self.lock['files'][0]]
        with self.assertRaisesRegex(M.EvidenceError, 'exact APKBUILD checksums'):
            M.check_recipe_inputs(self.lock, inputs)

    def test_archive_notice_links_duplicates_and_missing_members_are_rejected(self):
        for kind in ('symlink', 'duplicate'):
            data, _ = archive_bytes(kind)
            with self.subTest(kind=kind), self.assertRaises(M.EvidenceError):
                M.read_notices(self.lock, {(self.origin, 'fixture.tar.gz'): data}, self.args.licenses)
        self.lock['archiveNotices'][0]['member'] = 'source/MISSING'
        with self.assertRaises(M.EvidenceError):
            M.read_notices(self.lock, {(self.origin, 'fixture.tar.gz'): self.archive}, self.args.licenses)

    def test_symlinks_and_fifos_do_not_redirect_or_block_file_reads(self):
        link = self.root / 'link'
        link.symlink_to(self.args.installed)
        with self.assertRaises((M.EvidenceError, OSError)):
            M.regular(link, 10000)
        parent = self.root / 'linked-cache'
        parent.symlink_to(self.cache, target_is_directory=True)
        with self.assertRaises((M.EvidenceError, OSError)):
            M.regular(parent / self.origin / 'APKBUILD', 10000)
        fifo = self.root / 'fifo'
        os.mkfifo(fifo)
        with self.assertRaises((M.EvidenceError, OSError)):
            M.regular(fifo, 10000)

    def test_existing_output_is_preserved(self):
        self.args.output.mkdir()
        marker = self.args.output / 'keep'
        marker.write_text('existing')
        with self.assertRaises(M.EvidenceError):
            self.run_collector()
        self.assertEqual(marker.read_text(), 'existing')
        self.assertEqual(list(self.root.glob('.alpine-evidence-*')), [])

    def test_unchecked_recipe_sibling_cannot_be_dropped(self):
        sibling = {'name': 'package.post-install', 'mode': '100644', 'gitBlob': 'a' * 40}
        tree = self.lock['recipeDirectories'][0]
        tree['entries'].append(sibling)
        tree['treeSHA1'] = M.git_tree_digest(tree['entries'])
        self.save_lock()
        with self.assertRaisesRegex(M.EvidenceError, 'complete recipe Git tree'):
            M.load_lock(self.args.lock)
        tree['entries'].pop()
        self.save_lock()
        with self.assertRaisesRegex(M.EvidenceError, 'Git tree digest differs'):
            M.load_lock(self.args.lock)

    def test_declared_image_pins_require_digest_syntax(self):
        self.lock['baseIndexDigest'] = 'not-a-digest'
        self.save_lock()
        with self.assertRaisesRegex(M.EvidenceError, 'image pin is malformed'):
            M.load_lock(self.args.lock)

    def test_parent_swap_after_open_does_not_redirect_input(self):
        origin = self.cache / self.origin
        saved = self.cache / 'saved'
        outside = self.root / 'outside'
        outside.mkdir()
        (outside / 'APKBUILD').write_bytes(b'wrong input')
        original_open = os.open
        swapped = False
        def intercept(path, flags, *args, **kwargs):
            nonlocal swapped
            if path == 'APKBUILD' and kwargs.get('dir_fd') is not None and not swapped:
                swapped = True
                origin.rename(saved)
                origin.symlink_to(outside, target_is_directory=True)
            return original_open(path, flags, *args, **kwargs)
        with mock.patch.object(M.os, 'open', side_effect=intercept):
            self.assertEqual(M.regular(origin / 'APKBUILD', 10000), self.recipe)
        self.assertTrue(swapped)
        self.assertEqual((outside / 'APKBUILD').read_bytes(), b'wrong input')

    def test_source_symlink_preserves_only_an_included_regular_sibling(self):
        target = b'APKBUILD'
        digest = hashlib.sha1(b'blob ' + str(len(target)).encode() + b'\0' + target).hexdigest()
        self.lock['files'].append({'origin': self.origin, 'name': 'post-upgrade',
                                   'bytes': len(target), 'sha256': M.sha(target), 'gitBlob': digest})
        self.lock['recipeDirectories'][0]['entries'].append(
            {'name': 'post-upgrade', 'mode': '120000', 'gitBlob': digest})
        inputs = {(self.origin, 'APKBUILD'): self.recipe, (self.origin, 'fixture.tar.gz'): self.archive,
                  (self.origin, 'post-upgrade'): target}
        encoded, _ = M.source_bundle(self.lock, inputs)
        with tarfile.open(fileobj=io.BytesIO(encoded)) as archive:
            entry = archive.getmember('ca-certificates/post-upgrade')
            self.assertTrue(entry.issym())
            self.assertEqual(entry.linkname, 'APKBUILD')
            self.assertTrue(archive.getmember('ca-certificates/APKBUILD').isfile())
        for bad in (b'../outside', b'absent', b'post-upgrade'):
            with self.subTest(target=bad), self.assertRaises(M.EvidenceError):
                M.source_bundle(self.lock, {**inputs, (self.origin, 'post-upgrade'): bad})

    def test_source_size_bound_is_enforced_before_fetching(self):
        self.lock['files'][0]['bytes'] = M.MAX_FILE + 1
        self.save_lock()
        with self.assertRaises(M.EvidenceError):
            M.load_lock(self.args.lock)

    def test_lock_cannot_claim_different_or_unknown_bounds(self):
        for limits in ({}, {**M.LOCK_LIMITS, 'fileBytes': M.MAX_FILE + 1},
                       {**M.LOCK_LIMITS, 'extra': 1}, {**M.LOCK_LIMITS, 'fileBytes': float(M.MAX_FILE)}):
            self.lock['limits'] = limits
            self.save_lock()
            with self.assertRaisesRegex(M.EvidenceError, 'enforced bounds'):
                M.load_lock(self.args.lock)

    def test_output_parent_swap_never_writes_through_replacement(self):
        parent = self.root / 'publish'
        parent.mkdir()
        saved = self.root / 'saved-publish'
        outside = self.root / 'outside'
        outside.mkdir()
        output = parent / 'result'
        original_open = os.open
        swapped = False
        def intercept(path, flags, *args, **kwargs):
            nonlocal swapped
            if path == 'inventory.json' and kwargs.get('dir_fd') is not None and not swapped:
                swapped = True
                parent.rename(saved)
                parent.symlink_to(outside, target_is_directory=True)
            return original_open(path, flags, *args, **kwargs)
        with mock.patch.object(M.os, 'open', side_effect=intercept):
            with self.assertRaises((M.EvidenceError, OSError)):
                M.publish_output(output, {'inventory.json': b'proof'})
        self.assertTrue(swapped)
        self.assertEqual(list(outside.iterdir()), [])
        self.assertEqual(list(saved.iterdir()), [])

    def test_partial_output_write_is_cleaned_without_touching_other_files(self):
        keep = self.root / 'keep'
        keep.write_bytes(b'keep')
        with mock.patch.object(M.os, 'write', side_effect=OSError('synthetic failure')):
            with self.assertRaises(OSError):
                M.publish_output(self.args.output, {'inventory.json': b'proof'})
        self.assertFalse(self.args.output.exists())
        self.assertEqual(keep.read_bytes(), b'keep')


if __name__ == '__main__':
    unittest.main()
