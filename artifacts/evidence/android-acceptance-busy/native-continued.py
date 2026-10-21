"""Continue the retained ABI preflight; never reinstall or replay instrumentation."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT=Path(__file__).resolve().parents[3]
BASE=Path(__file__).resolve().parent
OUT=BASE/'native-continuation'
ADB=[str(Path.home()/'Library/Android/sdk/platform-tools/adb'),'-s','emulator-5606']
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

old_raw=(BASE/'native/receipt.json').read_bytes();old=json.loads(old_raw)
assert old['phase']=='failed-state-retained' and old['passed'] is False and 'instrumentationPidInformational' not in old
assert old['serial']=='emulator-5606' and old['avd']=='Interlink_Acceptance_API37' and old['syntheticOnly'] is True
assert adb('emu','avd','name').splitlines()[0]==old['avd']
assert shell('getprop ro.kernel.qemu')=='1' and shell('getprop ro.build.version.sdk')=='37'
assert shell('getprop sys.boot_completed')=='1' and shell('getprop ro.product.cpu.abi')=='arm64-v8a'
assert fingerprint()==old['before']
assert not OUT.exists();OUT.mkdir(mode=0o700)
sources={**old['sourceHashes'],'scripts/android-native-library-directory.py':sha((ROOT/'scripts/android-native-library-directory.py').read_bytes()),
    'scripts/android-instrumentation-result.sh':sha((ROOT/'scripts/android-instrumentation-result.sh').read_bytes())}
assert all(sha((ROOT/n).read_bytes())==h for n,h in sources.items())
report={'format':1,'serial':old['serial'],'avd':old['avd'],'syntheticOnly':True,'passed':False,
    'phase':'retained-installed-apks-admitted','before':old['before'],'sourceHashes':sources,
    'previousFailedReceiptSha256':sha(old_raw),'apkSha256':old['apkSha256'],'testApkSha256':old['testApkSha256'],
    'reinstalled':False,'physicalDeviceAccepted':False,'releaseReady':False}
def save():(OUT/'receipt.json').write_text(json.dumps(report,indent=2)+'\n')
def phase(value):report['phase']=value;save();print(json.dumps({'phase':value}),flush=True)
save()
try:
    for package,expected in ((PACKAGE,old['apkSha256']),(TEST_PACKAGE,old['testApkSha256'])):
        installed=shell('pm path '+package);assert installed.startswith('package:/data/app/') and len(installed.splitlines())==1
        assert shell('sha256sum '+installed[8:]).split()[0]==expected
    apk=ROOT/'apps/android/app/build/outputs/apk/debug/app-debug.apk';assert sha(apk.read_bytes())==old['apkSha256']
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
    assert fingerprint()==old['before']
    phase('first-instrumentation-running-observe-existing');started=time.monotonic()
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
    report['previousFailedReceiptUnchanged']=(BASE/'native/receipt.json').read_bytes()==old_raw
    assert code==0 and validation.returncode==0 and report['sourcesUnchanged'] and report['previousFailedReceiptUnchanged']
    assert report['after']==report['before']
    report['passed']=True;phase('real-saf-folder-journey-passed')
except BaseException as failure:
    report['failureClass']=type(failure).__name__;phase('failed-state-retained');raise
