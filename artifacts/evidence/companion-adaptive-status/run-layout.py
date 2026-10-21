"""One explicit component regression run on the retained, owned synthetic AVD."""
from pathlib import Path
import hashlib
import io
import json
import os
import re
import subprocess
import time
import zipfile

root = Path.cwd()
out = root / 'artifacts/evidence/companion-adaptive-status/layout'
out.mkdir()
adb_bin = '/Users/michaelwong/Library/Android/sdk/platform-tools/adb'
serial = 'emulator-5616'
package = 'life.michaelwong.interlink'
test = 'life.michaelwong.covalent.ui.InterlinkLayoutInstrumentedTest'
receipt = dict(serial=serial, syntheticOnly=True, physicalDeviceAccepted=False,
               dataCleared=False, instrumentationRetried=False, passed=False, releaseReady=False)

def call(*args):
    return subprocess.run(args, check=True, capture_output=True, timeout=45).stdout

def adb(*args):
    return call(adb_bin, '-s', serial, *args).decode().strip()

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def uid():
    matches = [line for line in adb('shell', 'cmd', 'package', 'list', 'packages', '-U', package).splitlines()
               if re.fullmatch('package:' + re.escape(package) + r' uid:[0-9]+', line)]
    assert len(matches) == 1
    return matches[0]

def framework():
    result = dict(bootId=adb('shell', 'cat', '/proc/sys/kernel/random/boot_id'))
    for name in ('system_server', 'surfaceflinger'):
        pid = adb('shell', 'pidof', name)
        assert re.fullmatch(r'[1-9][0-9]*', pid)
        stat = adb('shell', 'cat', '/proc/' + pid + '/stat')
        result[name] = pid + ':' + stat.rsplit(')', 1)[1].split()[19]
    return result

def identities():
    paths = ['no_backup/covalent-node/identity.json', 'no_backup/covalent-node/tls/identity.json',
             'no_backup/covalent-node/folder-sync/engine-identity.v1',
             'no_backup/covalent-node/network-pairing.json', 'no_backup/covalent-node/folder-sync/folder-sharing.v1']
    return {p: adb('shell', 'run-as', package, 'sha256sum', p).split()[0] for p in paths}

def installed_path(name):
    value = adb('shell', 'pm', 'path', name)
    assert re.fullmatch(r'package:/data/app/[A-Za-z0-9_~=/.-]+/base\.apk', value)
    return value.removeprefix('package:')

def natives(data):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        return {n: hashlib.sha256(archive.read(n)).hexdigest() for n in archive.namelist()
                if n.startswith('lib/') and n.endswith('.so')}

def save():
    (out / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')

try:
    assert adb('emu', 'avd', 'name').splitlines()[0] == 'Interlink_Companion_Address_Ready_API37'
    assert adb('shell', 'getprop', 'ro.kernel.qemu') == '1'
    assert 'InstrumentationRecord{' not in adb('shell', 'dumpsys', 'activity')
    receipt.update(before=framework(), uid=uid(), identitiesBefore=identities())
    assert receipt['before']['bootId'] == '1754fd1e-99b9-4966-a96b-4793c27dc8f3'
    assert receipt['uid'] == 'package:life.michaelwong.interlink uid:10232'
    old = call(adb_bin, '-s', serial, 'exec-out', 'cat', installed_path(package))
    receipt['previousApkSha256'] = hashlib.sha256(old).hexdigest()
    assert receipt['previousApkSha256'] == 'b2bb9709a4b0c2739bb832d3d99651d2e1959efbea16b80ef76e2461aa83df86'
    build = json.loads((out.parent / 'build.json').read_text())
    assert build['exitCode'] == 0 and build['sourcesUnchanged']
    receipt['sourceHashes'] = build['sourceHashes']
    assert all(digest(root / p) == h for p, h in receipt['sourceHashes'].items())
    app, tests = (root / build[n]['path'] for n in ('app', 'tests'))
    receipt.update(apkSha256=digest(app), testApkSha256=digest(tests), nativeHashes=natives(old))
    assert len(receipt['nativeHashes']) == 8 and receipt['nativeHashes'] == natives(app.read_bytes())
    os.environ['JAVA_HOME'] = '/opt/homebrew/opt/openjdk@17'
    sdk_tools = '/Users/michaelwong/Library/Android/sdk/build-tools/37.0.0/'
    for key, path, label in [('app', app, 'install.log'), ('tests', tests, 'install-test.log')]:
        assert digest(path) == build[key]['sha256']
        call(sdk_tools + 'zipalign', '-c', '-P', '16', '4', str(path))
        assert b'20a6923903ddb545a4458698925e2715e2d1b9df3d7dbc20d2fc927ea847018d' in call(sdk_tools + 'apksigner', 'verify', '--print-certs', str(path))
        result = adb('install', '-r', '--no-incremental', '--no-streaming', str(path))
        (out / label).write_text(result + '\n')
        assert result == 'Success'
    assert uid() == receipt['uid'] and framework() == receipt['before']
    assert identities() == receipt['identitiesBefore']
    assert adb('shell', 'sha256sum', installed_path(package)).split()[0] == receipt['apkSha256']
    assert adb('shell', 'sha256sum', installed_path(package + '.test')).split()[0] == receipt['testApkSha256']
    validator = root / 'scripts/android-instrumentation-result.sh'
    test_sources = root / 'apps/android/companion/src/androidTest'
    expected = call('sh', '-c', '. "$1"; derive_android_instrumentation_suite "$2"', '_', str(validator), str(test_sources)).decode()
    assert len(expected.splitlines()) == 2 and all(line.startswith(test + '#') for line in expected.splitlines())
    (out / 'expected.txt').write_text(expected)
    receipt['phase'] = 'component-regressions-running'
    save()
    start = time.monotonic()
    with (out / 'instrumentation.log').open('xb') as log:
        result = subprocess.run([adb_bin, '-s', serial, 'shell', 'am', 'instrument', '-w', '-r', '-e', 'class', test,
                                 package + '.test/androidx.test.runner.AndroidJUnitRunner'], stdout=log, stderr=subprocess.STDOUT,
                                timeout=180)
    receipt.update(instrumentationExitCode=result.returncode, seconds=round(time.monotonic()-start, 3),
                   after=framework(), identitiesAfter=identities())
    validation = subprocess.run(['sh', '-c', '. "$1"; validate_android_api37_result "$2" "$3"', '_', str(validator),
                                 str(out / 'instrumentation.log'), str(out / 'expected.txt')], capture_output=True)
    (out / 'validation.log').write_bytes(validation.stdout + validation.stderr)
    assert result.returncode == 0 and validation.returncode == 0
    assert receipt['after'] == receipt['before'] and receipt['identitiesAfter'] == receipt['identitiesBefore']
    assert all(digest(root / p) == h for p, h in receipt['sourceHashes'].items())
    receipt.update(passed=True, phase='two-component-regressions-passed', sourcesUnchanged=True)
except BaseException as failure:
    receipt.update(phase='failed-state-retained', failureClass=type(failure).__name__)
    raise
finally:
    save()
print(json.dumps({k:v for k,v in receipt.items() if k not in ('sourceHashes', 'nativeHashes')}))
