#!/usr/bin/env python3
"""Verify and package the website handoff from a clean Git checkout (stdlib only)."""
import hashlib
import json
import subprocess
import struct
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / 'docs' / 'website'


def git(*args):
    return subprocess.check_output(['git', *args], cwd=ROOT)


def main():
    if git('status', '--porcelain', '--untracked-files=normal').strip():
        raise SystemExit('Commit or preserve working changes before packaging; only committed source is included.')
    revision = git('rev-parse', 'HEAD').decode().strip()
    content = json.loads((WEB / 'content.json').read_text())
    assert len(content['screenshots']) == 5, 'Five reviewed screenshots are required'
    for item in content['screenshots']:
        path = WEB / item['file']
        assert path.is_file() and path.read_bytes().startswith(b'\xff\xd8'), path
        assert item['alt'] and item['caption'], path
    assert (WEB / content['social']['imagePath']).read_bytes().startswith(b'\x89PNG\r\n\x1a\n')
    for name in ('HANDOFF.md', 'copy.md', 'index.html', 'claims-and-sources.md',
                 'repo-review.md', 'ASSET-NOTICES.md', 'brand/covalent-mark.svg',
                 'brand/covalent-mark-light.svg', 'brand/social-card.svg'):
        assert (WEB / name).is_file(), name
    previews = json.loads((WEB / 'mockups/manifest.json').read_text())
    assert len(previews['assets']) == 5
    android = previews['androidCapture']
    capture = (WEB / 'mockups' / android['file']).read_bytes()
    assert capture.startswith(b'\x89PNG\r\n\x1a\n')
    assert struct.unpack('>II', capture[16:24]) == (1080, 2400)
    for item in previews['assets']:
        image = (WEB / 'mockups' / (item['file'] + '.png')).read_bytes()
        assert image.startswith(b'\x89PNG\r\n\x1a\n'), item['file']
        assert struct.unpack('>II', image[16:24]) == (item['width'], item['height']), item['file']
        webp = (WEB / 'mockups' / (item['file'] + '.webp')).read_bytes()
        assert webp[:4] == b'RIFF' and webp[8:12] == b'WEBP', item['file']

    # A Git archive includes only committed files, never local runtime state.
    archived = git('archive', '--format=zip', 'HEAD')
    import io
    entries = {}
    with zipfile.ZipFile(io.BytesIO(archived)) as source:
        for info in source.infolist():
            if not info.is_dir():
                entries['repository/' + info.filename] = (source.read(info), info.external_attr)
    start = f'''# Covalent website content pack

Open **repository/docs/website/HANDOFF.md** first.
Open **repository/docs/website/index.html** for the local screenshot gallery.
Open **repository/docs/website/mockups/index.html** for five promotional previews.

The pack contains five current WebUI screenshots with example data, editable
logos, a social image, complete website copy and metadata, a founder case study,
source notes, five PNG/WebP promotional compositions with editable layouts,
and the full polished repository source snapshot.

Website assets and copy: repository/docs/website/
Repository introduction: repository/README.md
Source commit: {revision}
Repository: https://github.com/thekozugroup/Covalent

This is a source/content handoff, not an installer or a new release. The source
snapshot does not imply its development branch has been merged into main.
No live server data, keys, deployment artifacts or build caches are included.
Server captures use example data. Framed phone previews use a real native Android
setup capture; the separate narrow server screenshot is mobile web. See the
preview asset notices for Google's Pixel frame and the generated background.

MANIFEST.json records file checksums and the source revision. To verify on
macOS/Linux, run `shasum -a 256 -c SHA256SUMS` from this directory.
'''
    entries['START-HERE.md'] = (start.encode(), 0o100644 << 16)
    checksums = {name: hashlib.sha256(body).hexdigest() for name, (body, _) in sorted(entries.items())}
    manifest = dict(schemaVersion=1, sourceCommit=revision,
                    screenshotSourceCommit='1b7a38cbd05e3fb1d585fc10c648b9bc2a4f9c7d',
                    capturedOn='2026-10-03', previewCreatedOn=previews['createdOn'],
                    exampleData=True, files=checksums)
    entries['MANIFEST.json'] = ((json.dumps(manifest, indent=2)+'\n').encode(), 0o100644 << 16)
    checksums['MANIFEST.json'] = hashlib.sha256(entries['MANIFEST.json'][0]).hexdigest()
    entries['SHA256SUMS'] = (''.join(f'{digest}  {name}\n' for name, digest in sorted(checksums.items())).encode(), 0o100644 << 16)
    output = ROOT / 'artifacts' / 'website' / 'Covalent-website-pack.zip'
    output.parent.mkdir(parents=True, exist_ok=True)
    archive_date = (*map(int, previews['createdOn'].split('-')), 0, 0, 0)
    with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
        for name, (body, mode) in sorted(entries.items()):
            info = zipfile.ZipInfo('Covalent-website-pack/' + name, archive_date)
            info.external_attr = mode
            info.compress_type = zipfile.ZIP_DEFLATED
            bundle.writestr(info, body)
    with zipfile.ZipFile(output) as bundle:
        assert bundle.testzip() is None
        for name, digest in checksums.items():
            assert hashlib.sha256(bundle.read('Covalent-website-pack/' + name)).hexdigest() == digest
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    output.with_suffix('.zip.sha256').write_text(f'{digest}  {output.name}\n')
    print(f'{output}\n{len(entries)} files, {output.stat().st_size:,} bytes; archive and SHA-256 checks passed.')


if __name__ == '__main__':
    main()
