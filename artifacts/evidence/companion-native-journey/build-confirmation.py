from pathlib import Path
import os,hashlib,json,subprocess,time
base=Path('artifacts/evidence/companion-native-journey/attempt-semantic-confirmation')
base.mkdir()
paths=json.loads(Path('artifacts/evidence/companion-native-journey/build.json').read_text())['sourceHashes']
hashes={p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in paths}
receipt={'command':['./apps/android/gradlew','-p','apps/android','--no-daemon','-PcovalentBuildSyncEngine=true',':companion:assembleDebugAndroidTest',':app:compileDebugAndroidTestKotlin'],'sourceHashes':hashes,'physicalDeviceAccepted':False,'releaseReady':False}
env=os.environ.copy()
env.update(JAVA_HOME='/opt/homebrew/opt/openjdk@17',ANDROID_HOME='/Users/michaelwong/Library/Android/sdk',COVALENT_ANDROID_NDK_HOME='/Users/michaelwong/Library/Android/sdk/ndk/27.1.12297006',GOROOT='/Users/michaelwong/Library/Caches/Covalent/toolchains/go1.26.10-0b5183492292/go')
env['PATH']=env['GOROOT']+'/bin:/opt/homebrew/opt/openjdk@17/bin:/opt/homebrew/bin:/Users/michaelwong/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'
start=time.monotonic()
with (base/'build.log').open('wb') as log:
 result=subprocess.run(receipt['command'],env=env,stdout=log,stderr=subprocess.STDOUT)
receipt.update(exitCode=result.returncode,seconds=round(time.monotonic()-start,3),sourcesUnchanged=all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in hashes.items()))
for key,p in [('companionApkSha256','apps/android/companion/build/outputs/apk/debug/companion-debug.apk'),('testApkSha256','apps/android/companion/build/outputs/apk/androidTest/debug/companion-debug-androidTest.apk')]:
 receipt[key]=hashlib.sha256(Path(p).read_bytes()).hexdigest()
(base/'build.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps(receipt))
raise SystemExit(result.returncode)
