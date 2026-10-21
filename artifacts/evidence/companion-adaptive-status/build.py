from pathlib import Path
import hashlib
import json
import os
import subprocess
import time

root = Path.cwd()
out = root / 'artifacts/evidence/companion-adaptive-status'
assert not (out / 'build.json').exists()
paths = sorted({*root.glob('apps/android/app/src/main/**/*.kt'),
                *root.glob('apps/android/app/src/main/res/**/*.xml'),
                *root.glob('apps/android/companion/src/**/*.kt'),
                root / 'apps/android/app/src/main/AndroidManifest.xml',
                root / 'apps/android/companion/src/main/AndroidManifest.xml',
                root / 'apps/android/companion/build.gradle.kts',
                root / 'apps/android/app/build.gradle.kts'})
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
sources = {str(p.relative_to(root)): digest(p) for p in paths}
command = ['./apps/android/gradlew', '-p', 'apps/android', '--no-daemon',
           '-PcovalentBuildSyncEngine=true', ':companion:assembleDebug',
           ':companion:assembleDebugAndroidTest', ':companion:lintDebug',
           ':app:compileDebugAndroidTestKotlin', ':app:lintDebug']
env = os.environ.copy()
env.update(JAVA_HOME='/opt/homebrew/opt/openjdk@17',
           ANDROID_HOME='/Users/michaelwong/Library/Android/sdk',
           COVALENT_ANDROID_NDK_HOME='/Users/michaelwong/Library/Android/sdk/ndk/27.1.12297006',
           GOROOT='/Users/michaelwong/Library/Caches/Covalent/toolchains/go1.26.10-0b5183492292/go')
env['PATH'] = env['GOROOT'] + '/bin:/opt/homebrew/opt/openjdk@17/bin:/opt/homebrew/bin:/Users/michaelwong/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'
receipt = dict(command=command, sourceHashes=sources, releaseReady=False, physicalDeviceAccepted=False)
start = time.monotonic()
with (out / 'build.log').open('xb') as log:
    result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
receipt.update(exitCode=result.returncode, seconds=round(time.monotonic()-start, 3),
               sourcesUnchanged=all(digest(root / p) == h for p, h in sources.items()))
for name, path in [('app', 'apps/android/companion/build/outputs/apk/debug/companion-debug.apk'),
                   ('tests', 'apps/android/companion/build/outputs/apk/androidTest/debug/companion-debug-androidTest.apk')]:
    receipt[name] = dict(path=path, sha256=digest(root / path))
(out / 'build.json').write_text(json.dumps(receipt, indent=2)+'\n')
print(json.dumps({k:v for k,v in receipt.items() if k != 'sourceHashes'}))
raise SystemExit(result.returncode)
