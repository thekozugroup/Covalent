import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT=Path(__file__).resolve().parents[3]
OUT=Path(__file__).resolve().parent/'native'
SDK=Path.home()/'Library/Android/sdk'
SERIAL='emulator-5606'
AVD='Interlink_Acceptance_API37'
PACKAGE='life.michaelwong.covalent'
TEST_PACKAGE=PACKAGE+'.test'
CLASS=PACKAGE+'.node.SafFolderSyncJourneyInstrumentedTest'
METHOD='nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant'
ADB=[str(SDK/'platform-tools/adb'),'-s',SERIAL]
SOURCES=('apps/android/app/src/main/java/life/michaelwong/covalent/data/CovalentNodeClient.kt',
    'apps/android/app/src/main/java/life/michaelwong/covalent/data/FolderAcceptanceRetry.kt',
    'apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt',
    'apps/android/app/build.gradle.kts','apps/android/companion/build.gradle.kts',
    'crates/covalent-node/src/sync_api.rs','crates/covalent-node/src/sync_engine/service.rs',
    'crates/covalent-node/src/sync_engine/sharing.rs')

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

assert adb('emu','avd','name').splitlines()[0]==AVD
assert shell('getprop ro.kernel.qemu')=='1' and shell('getprop ro.build.version.sdk')=='37'
assert shell('getprop sys.boot_completed')=='1'
assert shell('settings get global device_provisioned')=='1' and shell('settings get secure user_setup_complete')=='1'
assert shell('pm list packages '+PACKAGE)==''
assert not OUT.exists();OUT.mkdir(mode=0o700)
sources={n:sha((ROOT/n).read_bytes()) for n in SOURCES}
apk=ROOT/'apps/android/app/build/outputs/apk/debug/app-debug.apk'
test=ROOT/'apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk'
report={'format':1,'serial':SERIAL,'avd':AVD,'syntheticOnly':True,'passed':False,'phase':'fresh-fixture-admitted',
    'sourceHashes':sources,'apkSha256':sha(apk.read_bytes()),'testApkSha256':sha(test.read_bytes()),
    'before':fingerprint(),'physicalDeviceAccepted':False,'releaseReady':False}
def save():
    (OUT/'receipt.json').write_text(json.dumps(report,indent=2)+'\n')
def phase(value):report['phase']=value;save();print(json.dumps({'phase':value}),flush=True)
save()
try:
    for path in (apk,test):
        phase('install-'+path.name+'-intent')
        result=adb('install','-t','--no-incremental','--no-streaming',str(path))
        assert 'Success' in result
    phase('installed')
    shell('pm enable '+TEST_PACKAGE)
    for package,expected in ((PACKAGE,report['apkSha256']),(TEST_PACKAGE,report['testApkSha256'])):
        installed=shell('pm path '+package);assert installed.startswith('package:/data/app/') and len(installed.splitlines())==1
        assert shell('sha256sum '+installed[8:]).split()[0]==expected
    installed=shell('pm path '+PACKAGE)+'\n'
    native=subprocess.check_output([sys.executable,'-B',str(ROOT/'scripts/android-native-library-directory.py'),str(apk)],input=installed.encode()).decode().splitlines()
    report['nativeLibraryDirectory']=native[0];report['nativeHelpers']={}
    for line in native[1:]:
        expected,name=line.split();assert name in ('libcovalentrclone.so','libengineguardian.so')
        remote=native[0]+'/'+name
        adb('shell','-T','run-as',PACKAGE,'test','-x',remote)
        assert adb('shell','-T','run-as',PACKAGE,'/system/bin/sha256sum',remote).split()[0]==expected
        report['nativeHelpers'][name]=expected
    assert len(report['nativeHelpers'])==2
    shell('pm grant '+PACKAGE+' android.permission.ACCESS_LOCAL_NETWORK')
    assert fingerprint()==report['before']
    phase('instrumentation-running-observe-existing');started=time.monotonic()
    with (OUT/'instrumentation.log').open('x') as log:
        process=subprocess.Popen(ADB+['shell','am','instrument','-w','-r','-e','class',CLASS,
            TEST_PACKAGE+'/androidx.test.runner.AndroidJUnitRunner'],stdout=log,stderr=subprocess.STDOUT)
        report['instrumentationPidInformational']=process.pid;save();code=process.wait()
    report['instrumentationExitCode']=code;report['seconds']=round(time.monotonic()-started,3)
    expected=OUT/'expected.tsv';expected.write_text(CLASS+'\t'+METHOD+'\n')
    validation=subprocess.run(['/bin/sh','-c','. "$1"; validate_android_api37_result "$2" "$3"','native-result',
        str(ROOT/'scripts/android-instrumentation-result.sh'),str(OUT/'instrumentation.log'),str(expected)],
        stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    (OUT/'validation.log').write_bytes(validation.stdout)
    report['resultValidationExitCode']=validation.returncode
    report['after']=fingerprint();report['sourcesUnchanged']=all(sha((ROOT/n).read_bytes())==h for n,h in sources.items())
    assert code==0 and validation.returncode==0 and report['sourcesUnchanged'] is True and report['after']==report['before']
    report['passed']=True;phase('real-saf-folder-journey-passed')
except BaseException as failure:
    report['failureClass']=type(failure).__name__;phase('failed-state-retained');raise
