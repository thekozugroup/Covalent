"""Verify the corrected chooser observation after the original fresh-fixture attempt is terminal."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT=Path(__file__).resolve().parents[3]
BASE=Path(__file__).resolve().parent
OUT=BASE/'native-picker-readiness'
ADB=[str(Path.home()/'Library/Android/sdk/platform-tools/adb'),'-s','emulator-5608']
PACKAGE='life.michaelwong.covalent'
TEST_PACKAGE=PACKAGE+'.test'
CLASS=PACKAGE+'.node.SafFolderSyncJourneyInstrumentedTest'
METHOD='nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant'
def sha(data):return hashlib.sha256(data).hexdigest()
def adb(*args):return subprocess.check_output(ADB+list(args),stderr=subprocess.STDOUT).decode().strip()
def shell(command):return adb('shell',command)
def fingerprint():
    result={'bootId':shell('cat /proc/sys/kernel/random/boot_id'),'framework':{}}
    for name in ('system_server','surfaceflinger'):
        pid=shell('pidof '+name);assert pid.isdecimal() and int(pid)>0
        raw=shell('cat /proc/'+pid+'/stat');at=raw.rfind(') ');fields=raw[at+2:].split()
        assert at>=0 and len(fields)>=20 and fields[19].isdecimal() and fields[0] not in ('Z','X','x')
        result['framework'][name]=pid+':'+fields[19]
    return result

old_raw=(BASE/'native-final-fresh/receipt.json').read_bytes();old=json.loads(old_raw)
assert old['phase']=='failed-state-retained' and old['passed'] is False
assert old['serial']=='emulator-5608' and old['avd']=='Interlink_Strict_API37' and old['syntheticOnly'] is True
assert adb('emu','avd','name').splitlines()[0]==old['avd']
assert shell('getprop ro.kernel.qemu')=='1' and shell('getprop ro.build.version.sdk')=='37'
assert shell('getprop sys.boot_completed')=='1' and shell('getprop ro.product.cpu.abi')=='arm64-v8a'
assert fingerprint()==old['after']
assert not OUT.exists();OUT.mkdir(mode=0o700)
changed={'apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt'}
assert all(sha((ROOT/n).read_bytes())==h for n,h in old['sourceHashes'].items() if n not in changed)
sources={n:sha((ROOT/n).read_bytes()) for n in old['sourceHashes']}
apk=ROOT/'apps/android/app/build/outputs/apk/debug/app-debug.apk'
test=ROOT/'apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk'
report={'format':1,'serial':old['serial'],'avd':old['avd'],'syntheticOnly':True,'passed':False,
    'phase':'chooser-observation-update-admitted','before':old['after'],'sourceHashes':sources,
    'previousFailedReceiptSha256':sha(old_raw),'apkSha256':sha(apk.read_bytes()),'testApkSha256':sha(test.read_bytes()),
    'appReinstalled':False,'testUpdated':False,'dataCleared':False,'physicalDeviceAccepted':False,'releaseReady':False}
def save():(OUT/'receipt.json').write_text(json.dumps(report,indent=2)+'\n')
def phase(value):report['phase']=value;save();print(json.dumps({'phase':value}),flush=True)
save()
try:
    for package,expected in ((PACKAGE,old['apkSha256']),(TEST_PACKAGE,old['testApkSha256'])):
        installed=shell('pm path '+package);assert installed.startswith('package:/data/app/') and len(installed.splitlines())==1
        assert shell('sha256sum '+installed[8:]).split()[0]==expected
    assert report['apkSha256']==old['apkSha256'] and report['testApkSha256']!=old['testApkSha256']
    for file,name in ((test,'test-install.log'),):
        update=subprocess.run(ADB+['install','-r','-t','--no-incremental','--no-streaming',str(file)],
            stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
        (OUT/name).write_bytes(update.stdout)
        assert update.returncode==0 and b'Success' in update.stdout
    report['testUpdated']=True
    for package,expected in ((PACKAGE,report['apkSha256']),(TEST_PACKAGE,report['testApkSha256'])):
        installed=shell('pm path '+package);assert installed.startswith('package:/data/app/') and len(installed.splitlines())==1
        assert shell('sha256sum '+installed[8:]).split()[0]==expected
    native=subprocess.check_output([sys.executable,'-B',str(ROOT/'scripts/android-native-library-directory.py'),str(apk),
        '--abi','arm64-v8a'],input=(shell('pm path '+PACKAGE)+'\n').encode()).decode().splitlines()
    report['nativeLibraryDirectory']=native[0];report['nativeHelpers']={}
    for line in native[1:]:
        expected,name=line.split();assert name in ('libcovalentrclone.so','libengineguardian.so')
        remote=native[0]+'/'+name
        adb('shell','-T','run-as',PACKAGE,'test','-x',remote)
        assert adb('shell','-T','run-as',PACKAGE,'/system/bin/sha256sum',remote).split()[0]==expected
        report['nativeHelpers'][name]=expected
    assert len(report['nativeHelpers'])==2
    shell('pm grant '+PACKAGE+' android.permission.ACCESS_LOCAL_NETWORK')
    assert fingerprint()==old['after']
    phase('chooser-observation-instrumentation-running-observe-existing');started=time.monotonic()
    with (OUT/'instrumentation.log').open('x') as log:
        process=subprocess.Popen(ADB+['shell','am','instrument','-w','-r','-e','class',CLASS,
            TEST_PACKAGE+'/androidx.test.runner.AndroidJUnitRunner'],stdout=log,stderr=subprocess.STDOUT)
        report['instrumentationPidInformational']=process.pid;save();code=process.wait()
    report['instrumentationExitCode']=code;report['seconds']=round(time.monotonic()-started,3)
    expected=OUT/'expected.txt';expected.write_text(CLASS+'#'+METHOD+'\n')
    validation=subprocess.run(['/bin/sh','-c','. "$1"; validate_android_api37_result "$2" "$3"','native-result',
        str(ROOT/'scripts/android-instrumentation-result.sh'),str(OUT/'instrumentation.log'),str(expected)],
        stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    (OUT/'validation.log').write_bytes(validation.stdout)
    report['resultValidationExitCode']=validation.returncode;report['after']=fingerprint()
    report['sourcesUnchanged']=all(sha((ROOT/n).read_bytes())==h for n,h in sources.items())
    report['previousFailedReceiptUnchanged']=(BASE/'native-final-fresh/receipt.json').read_bytes()==old_raw
    assert code==0 and validation.returncode==0 and report['sourcesUnchanged'] and report['previousFailedReceiptUnchanged']
    assert report['after']==report['before']
    report['passed']=True;phase('real-saf-folder-journey-passed')
except BaseException as failure:
    report['failureClass']=type(failure).__name__;phase('failed-state-retained');raise
