#!/usr/bin/env python3
"""One explicit confirmation-corrected journey on another new owned AVD."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
ADB = "/Users/michaelwong/Library/Android/sdk/platform-tools/adb"
SERIAL = "emulator-5614"
AVD = "Interlink_Companion_Confirm_API37"
PACKAGE = "life.michaelwong.interlink"
TEST = "life.michaelwong.covalent.node.SafFolderSyncJourneyInstrumentedTest"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(*args):
    return subprocess.run(args, cwd=ROOT, capture_output=True, check=True, timeout=45).stdout.decode().strip()


def adb(*args):
    return run(ADB, "-s", SERIAL, *args)


def framework():
    value = {"bootId": adb("shell", "cat", "/proc/sys/kernel/random/boot_id")}
    for name in ("system_server", "surfaceflinger"):
        pid = adb("shell", "pidof", name)
        assert re.fullmatch(r"[1-9][0-9]*", pid)
        stat = adb("shell", "cat", "/proc/" + pid + "/stat")
        value[name] = pid + ":" + stat.rsplit(")", 1)[1].split()[19]
    return value


def exact_uid():
    records = adb("shell", "cmd", "package", "list", "packages", "-U", PACKAGE).splitlines()
    exact = [line for line in records if re.fullmatch("package:" + re.escape(PACKAGE) + r" uid:[0-9]+", line)]
    assert len(exact) == 1
    return exact[0]


def installed_digest(package):
    output = adb("shell", "pm", "path", package)
    match = re.fullmatch(r"package:(/data/app/(?:~~[A-Za-z0-9_=-]+/)?" + re.escape(package) + r"-[A-Za-z0-9_=-]+/base\.apk)", output)
    assert match
    return adb("shell", "sha256sum", match[1]).split()[0]


def main():
    work = HERE / "native-semantic-confirmation"
    work.mkdir()
    receipt = {"format": 1, "serial": SERIAL, "avd": AVD, "syntheticOnly": True,
               "passed": False, "phase": "preflight", "dataCleared": False,
               "instrumentationRetried": False, "newAttemptAfterTestAndPickerCorrection": True,
               "physicalDeviceAccepted": False, "releaseReady": False}
    def save():
        (work / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    save()
    try:
        assert adb("get-state") == "device"
        assert adb("emu", "avd", "name").splitlines()[0] == AVD
        assert adb("shell", "getprop", "ro.kernel.qemu") == "1"
        assert adb("shell", "getprop", "ro.build.version.sdk") == "37"
        assert adb("shell", "getprop", "sys.boot_completed") == "1"
        receipt["before"] = framework()
        receipt["systemFingerprint"] = adb("shell", "getprop", "ro.build.fingerprint")
        assert receipt["systemFingerprint"] == run(ADB, "-s", "emulator-5610", "shell", "getprop", "ro.build.fingerprint")
        assert not adb("shell", "cmd", "package", "list", "packages", "-U", PACKAGE)
        build = json.loads((HERE / "attempt-semantic-confirmation/build.json").read_text())
        assert build["exitCode"] == 0 and build["sourcesUnchanged"]
        receipt["sourceHashes"] = build["sourceHashes"]
        extra = ["scripts/android-native-library-directory.py", "scripts/android-instrumentation-result.sh",
                 "apps/android/app/src/debug/java/life/michaelwong/covalent/node/FolderSyncInstrumentationBridge.kt"]
        receipt["sourceHashes"].update({p: digest(ROOT / p) for p in extra})
        assert all(digest(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        previous = ["native/receipt.json", "native-after-installed-update/receipt.json", "native-after-installed-update/instrumentation.log",
                    "attempt-photo-policy-assumption/SafFolderSyncJourneyInstrumentedTest.kt"]
        receipt["previousEvidence"] = {p: digest(HERE / p) for p in previous}
        assert digest(HERE / previous[-1]) == json.loads((HERE / previous[1]).read_text())["sourceHashes"][
            "apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"]
        earlier = ["native-photo-retention/receipt.json", "native-photo-retention/instrumentation.log",
                   "attempt-coordinate-confirmation/SafFolderSyncJourneyInstrumentedTest.kt"]
        receipt["previousEvidence"].update({p: digest(HERE / p) for p in earlier})
        assert json.loads((HERE / earlier[0]).read_text())["phase"] == "failed-state-retained"
        assert digest(HERE / earlier[2]) == json.loads((HERE / earlier[0]).read_text())["sourceHashes"][
            "apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"]
        apk = ROOT / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
        test_apk = ROOT / "apps/android/companion/build/outputs/apk/androidTest/debug/companion-debug-androidTest.apk"
        receipt.update(apkSha256=digest(apk), testApkSha256=digest(test_apk))
        assert receipt["apkSha256"] == build["companionApkSha256"] == "b2bb9709a4b0c2739bb832d3d99651d2e1959efbea16b80ef76e2461aa83df86"
        assert receipt["testApkSha256"] == build["testApkSha256"]
        os.environ["JAVA_HOME"] = "/opt/homebrew/opt/openjdk@17"
        tools = "/Users/michaelwong/Library/Android/sdk/build-tools/37.0.0/"
        for path in (apk, test_apk):
            run(tools + "zipalign", "-c", "-P", "16", "4", str(path))
            assert "20a6923903ddb545a4458698925e2715e2d1b9df3d7dbc20d2fc927ea847018d" in run(tools + "apksigner", "verify", "--print-certs", str(path))
        receipt["packageAlignmentAndDebugSignatureVerified"] = True
        for path, name in [(apk, "install.log"), (test_apk, "install-test.log")]:
            output = adb("install", "--no-incremental", "--no-streaming", str(path))
            (work / name).write_text(output + "\n")
            assert "Success" in output
        receipt["uid"] = exact_uid()
        assert installed_digest(PACKAGE) == receipt["apkSha256"]
        assert installed_digest(PACKAGE + ".test") == receipt["testApkSha256"]
        adb("shell", "pm", "grant", PACKAGE, "android.permission.ACCESS_LOCAL_NETWORK")
        adb("shell", "settings", "put", "global", "device_provisioned", "1")
        adb("shell", "settings", "put", "secure", "user_setup_complete", "1")
        adb("shell", "input", "keyevent", "KEYCODE_WAKEUP")
        adb("shell", "wm", "dismiss-keyguard")
        assert framework() == receipt["before"]
        spec = importlib.util.spec_from_file_location("native_directory", ROOT / extra[0])
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        directory, helpers = module.resolve(apk, (adb("shell", "pm", "path", PACKAGE) + "\n").encode(), "arm64-v8a", PACKAGE)
        for name, expected in helpers.items():
            adb("shell", "run-as", PACKAGE, "test", "-x", directory + "/" + name)
            assert adb("shell", "run-as", PACKAGE, "sha256sum", directory + "/" + name).split()[0] == expected
        receipt["nativeHelpers"] = helpers
        generated = list((ROOT / "apps/android/companion/build/generated").rglob("SafFolderSyncJourneyInstrumentedTest.kt"))
        assert len(generated) == 1
        source = ROOT / "apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"
        assert generated[0].read_bytes() == source.read_bytes()
        validator = ROOT / extra[1]
        expected = run("sh", "-c", '. "$1"; derive_android_instrumentation_suite "$2"', "_", str(validator), str(generated[0].parent))
        assert expected == TEST + "#nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant"
        (work / "expected.txt").write_text(expected + "\n")
        receipt["phase"] = "native-journey-running"
        save()
        start = time.monotonic()
        with (work / "instrumentation.log").open("wb") as output:
            result = subprocess.run([ADB, "-s", SERIAL, "shell", "am", "instrument", "-w", "-r", "-e", "class", TEST,
                                     PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"], stdout=output, stderr=subprocess.STDOUT)
        receipt["seconds"] = round(time.monotonic() - start, 3)
        receipt["instrumentationExitCode"] = result.returncode
        receipt["after"] = framework()
        receipt["sourceHashesUnchanged"] = all(digest(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        receipt["previousEvidenceUnchanged"] = all(digest(HERE / p) == h for p, h in receipt["previousEvidence"].items())
        markers = "\n".join(line for line in adb("logcat", "-d", "-v", "threadtime", "System.out:I", "*:S").splitlines() if "COVALENT_" in line)
        (work / "markers.log").write_text(markers + "\n")
        validation = subprocess.run(["sh", "-c", '. "$1"; validate_android_api37_result "$2" "$3"', "_", str(validator), str(work / "instrumentation.log"), str(work / "expected.txt")], cwd=ROOT, capture_output=True)
        (work / "validation.log").write_bytes(validation.stdout + validation.stderr)
        assert validation.returncode == 0, "Strict instrumentation result validator refused the actual result"
        for marker in ["COVALENT_COMPANION_SOURCE_SETUP_PEER_OFFLINE=true", "COVALENT_JOURNEY_SURFACE=interlink-expressive-pair-first",
                       "COVALENT_COMPANION_DESTINATION_DELETION_STAYS_DELETED=true", "COVALENT_COMPANION_SOURCE_DELETION_KEEPS_DESTINATION=true", "COVALENT_SYSTEM_PICKER_CONFIRMATION_ACTION=accepted"]:
            assert marker in markers
        assert result.returncode == 0 and receipt["before"] == receipt["after"]
        assert receipt["sourceHashesUnchanged"] and receipt["previousEvidenceUnchanged"]
        assert exact_uid() == receipt["uid"] and installed_digest(PACKAGE) == receipt["apkSha256"]
        receipt.update(passed=True, phase="companion-native-saf-retention-journey-passed")
    except BaseException as failure:
        receipt.update(phase="failed-state-retained", failureClass=type(failure).__name__)
        save()
        raise
    finally:
        save()
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
