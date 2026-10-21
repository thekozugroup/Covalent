"""Continue after successful app update; no instrumentation has run yet."""
from pathlib import Path
import hashlib
import json
import os
import re
import subprocess
import time

root = Path.cwd()
base = root / 'artifacts/evidence/companion-adaptive-status'
out = base / 'layout-after-install'
out.mkdir()
prior = json.loads((base / 'layout/receipt.json').read_text())
assert prior['phase'] == 'failed-state-retained' and 'instrumentationExitCode' not in prior
assert not (base / 'layout/instrumentation.log').exists()
assert (base / 'layout/install.log').read_text().splitlines() == ['Performing Push Install', 'Success']
assert not (base / 'layout/install-test.log').exists()
receipt = dict(priorInstallReceiptSha256=hashlib.sha256((base / 'layout/receipt.json').read_bytes()).hexdigest(),
               sourceHashes=prior['sourceHashes'], passed=False, dataCleared=False, instrumentationRetried=False,
               continuationReason='adb push-install success has a progress heading; test install and first instrumentation still pending',
               releaseReady=False, physicalDeviceAccepted=False)
adb_bin = '/Users/michaelwong/Library/Android/sdk/platform-tools/adb'
serial = 'emulator-5616'
package = 'life.michaelwong.interlink'

def call(*args):
    return subprocess.run(args, check=True, capture_output=True, timeout=45).stdout

def adb(*args):
    return call(adb_bin, '-s', serial, *args).decode().strip()

def framework():
    result = dict(bootId=adb('shell', 'cat', '/proc/sys/kernel/random/boot_id'))
    for name in ('system_server', 'surfaceflinger'):
        pid = adb('shell', 'pidof', name)
        assert re.fullmatch(r'[1-9][0-9]*', pid)
        result[name] = pid + ':' + adb('shell', 'cat', '/proc/' + pid + '/stat').rsplit(')', 1)[1].split()[19]
    return result

def preserved():
    return {p: adb('shell', 'run-as', package, 'sha256sum', p).split()[0] for p in prior['identitiesBefore']}

def installed(name):
    path = adb('shell', 'pm', 'path', name)
    assert re.fullmatch(r'package:/data/app/[A-Za-z0-9_~=/.-]+/base\.apk', path)
    return adb('shell', 'sha256sum', path.removeprefix('package:')).split()[0]

def save():
    (out / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')

try:
    assert adb('emu', 'avd', 'name').splitlines()[0] == 'Interlink_Companion_Address_Ready_API37'
    assert adb('shell', 'getprop', 'ro.kernel.qemu') == '1'
    assert 'InstrumentationRecord{' not in adb('shell', 'dumpsys', 'activity')
    assert framework() == prior['before'] and preserved() == prior['identitiesBefore']
    assert prior['uid'] in adb('shell', 'cmd', 'package', 'list', 'packages', '-U', package).splitlines()
    assert installed(package) == prior['apkSha256']
    assert all(hashlib.sha256((root / p).read_bytes()).hexdigest() == h for p, h in receipt['sourceHashes'].items())
    build = json.loads((base / 'build.json').read_text())
    test_apk = root / build['tests']['path']
    assert hashlib.sha256(test_apk.read_bytes()).hexdigest() == prior['testApkSha256'] == build['tests']['sha256']
    os.environ['JAVA_HOME'] = '/opt/homebrew/opt/openjdk@17'
    sdk_tools = '/Users/michaelwong/Library/Android/sdk/build-tools/37.0.0/'
    call(sdk_tools + 'zipalign', '-c', '-P', '16', '4', str(test_apk))
    assert b'20a6923903ddb545a4458698925e2715e2d1b9df3d7dbc20d2fc927ea847018d' in call(sdk_tools + 'apksigner', 'verify', '--print-certs', str(test_apk))
    installed_test = adb('install', '-r', '--no-incremental', '--no-streaming', str(test_apk))
    (out / 'install-test.log').write_text(installed_test + '\n')
    assert installed_test.splitlines() in (['Success'], ['Performing Push Install', 'Success'])
    assert installed(package + '.test') == prior['testApkSha256']
    validator = root / 'scripts/android-instrumentation-result.sh'
    expected = call('sh', '-c', '. "$1"; derive_android_instrumentation_suite "$2"', '_', str(validator),
                    str(root / 'apps/android/companion/src/androidTest')).decode()
    test = 'life.michaelwong.covalent.ui.InterlinkLayoutInstrumentedTest'
    assert len(expected.splitlines()) == 2 and all(line.startswith(test + '#') for line in expected.splitlines())
    (out / 'expected.txt').write_text(expected)
    receipt.update(phase='first-component-instrumentation-running', before=framework(), identitiesBefore=preserved())
    save()
    start = time.monotonic()
    with (out / 'instrumentation.log').open('xb') as log:
        result = subprocess.run([adb_bin, '-s', serial, 'shell', 'am', 'instrument', '-w', '-r', '-e', 'class', test,
                                 package + '.test/androidx.test.runner.AndroidJUnitRunner'], stdout=log, stderr=subprocess.STDOUT,
                                timeout=180)
    receipt.update(instrumentationExitCode=result.returncode, seconds=round(time.monotonic()-start, 3),
                   after=framework(), identitiesAfter=preserved())
    validation = subprocess.run(['sh', '-c', '. "$1"; validate_android_api37_result "$2" "$3"', '_', str(validator),
                                 str(out / 'instrumentation.log'), str(out / 'expected.txt')], capture_output=True)
    (out / 'validation.log').write_bytes(validation.stdout + validation.stderr)
    assert result.returncode == 0 and validation.returncode == 0
    assert receipt['after'] == receipt['before'] and receipt['identitiesAfter'] == receipt['identitiesBefore']
    assert all(hashlib.sha256((root / p).read_bytes()).hexdigest() == h for p, h in receipt['sourceHashes'].items())
    receipt.update(passed=True, sourcesUnchanged=True, phase='two-component-regressions-passed')
except BaseException as failure:
    receipt.update(phase='failed-state-retained', failureClass=type(failure).__name__)
    raise
finally:
    save()
print(json.dumps({k:v for k,v in receipt.items() if k != 'sourceHashes'}))
