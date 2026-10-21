#!/usr/bin/env python3
"""One explicit native companion journey on the retained, synthetic-only AVD.

This frozen evidence runner never clears data, retries instrumentation or reboots.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[3]
EVIDENCE = Path(__file__).resolve().parent
ADB = "/Users/michaelwong/Library/Android/sdk/platform-tools/adb"
SERIAL = "emulator-5610"
PACKAGE = "life.michaelwong.interlink"
TEST = "life.michaelwong.covalent.node.SafFolderSyncJourneyInstrumentedTest"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(*args):
    result = subprocess.run(args, cwd=ROOT, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=45, check=True)
    return result.stdout.decode().strip()


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


def main():
    build = json.loads((EVIDENCE / "build.json").read_text())
    assert build["exitCode"] == 0 and build["sourcesUnchanged"]
    work = EVIDENCE / "native"
    work.mkdir()  # Existing attempts are immutable; never replay one.
    receipt = {"format": 1, "serial": SERIAL, "avd": "Interlink_Address_API37",
               "syntheticOnly": True, "passed": False, "phase": "preflight",
               "dataCleared": False, "instrumentationRetried": False,
               "physicalDeviceAccepted": False, "releaseReady": False}
    def save():
        (work / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    save()
    try:
        assert adb("get-state") == "device"
        assert adb("emu", "avd", "name").splitlines()[0] == "Interlink_Address_API37"
        assert adb("shell", "getprop", "ro.kernel.qemu") == "1"
        assert adb("shell", "getprop", "ro.build.version.sdk") == "37"
        receipt["before"] = framework()
        assert receipt["before"]["bootId"] == "8e6f07a7-a712-48b0-8812-22679d2f12c1"
        receipt["beforeUid"] = adb("shell", "cmd", "package", "list", "packages", "-U", PACKAGE)
        assert receipt["beforeUid"] == "package:" + PACKAGE + " uid:10234"
        assert "granted=true" in adb("shell", "dumpsys", "package", PACKAGE).split(
            "android.permission.ACCESS_LOCAL_NETWORK:", 1)[1].splitlines()[0]
        previous = ["artifacts/evidence/interlink-progressive-setup/ui.json",
                    "artifacts/evidence/android-acceptance-busy/native-picker-readiness/receipt.json",
                    "artifacts/evidence/android-address-recovery/native-packaged-diagnostic/receipt.json"]
        receipt["previousReceipts"] = {p: digest(ROOT / p) for p in previous}
        installed = adb("shell", "pm", "path", PACKAGE)
        match = re.fullmatch(r"package:(/data/app/(?:~~[A-Za-z0-9_=-]+/)?life\.michaelwong\.interlink-[A-Za-z0-9_=-]+/base\.apk)", installed)
        assert match
        receipt["beforeApkSha256"] = adb("shell", "sha256sum", match[1]).split()[0]
        assert receipt["beforeApkSha256"] == "d357b315b9105393fedf484789212ed3a2179c3929e6ce17a969942827ea783a"
        apk = ROOT / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
        test_apk = ROOT / "apps/android/companion/build/outputs/apk/androidTest/debug/companion-debug-androidTest.apk"
        receipt["apkSha256"] = digest(apk)
        receipt["testApkSha256"] = digest(test_apk)
        os.environ["JAVA_HOME"] = "/opt/homebrew/opt/openjdk@17"
        tools = "/Users/michaelwong/Library/Android/sdk/build-tools/37.0.0/"
        for path in (apk, test_apk):
            run(tools + "zipalign", "-c", "-P", "16", "4", str(path))
            certificate = run(tools + "apksigner", "verify", "--print-certs", str(path))
            assert "20a6923903ddb545a4458698925e2715e2d1b9df3d7dbc20d2fc927ea847018d" in certificate
        receipt["packageAlignmentAndDebugSignatureVerified"] = True
        extra = ["scripts/android-native-library-directory.py", "scripts/android-instrumentation-result.sh",
                 "apps/android/app/src/debug/java/life/michaelwong/covalent/node/FolderSyncInstrumentationBridge.kt"]
        receipt["sourceHashes"] = dict(build["sourceHashes"])
        receipt["sourceHashes"].update({p: digest(ROOT / p) for p in extra})
        assert all(digest(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        activity = adb("shell", "dumpsys", "activity", "instrumentation")
        assert "Active instrumentation" not in activity or "mFinished=false" not in activity
        receipt["phase"] = "verified-debug-update"
        save()
        for path, name in [(apk, "install.log"), (test_apk, "install-test.log")]:
            output = adb("install", "-r", "--no-incremental", "--no-streaming", str(path))
            (work / name).write_text(output + "\n")
            assert "Success" in output
        assert framework() == receipt["before"]
        assert adb("shell", "cmd", "package", "list", "packages", "-U", PACKAGE) == receipt["beforeUid"]
        spec = importlib.util.spec_from_file_location("native_directory", ROOT / extra[0])
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        native_dir, helpers = module.resolve(apk, (adb("shell", "pm", "path", PACKAGE) + "\n").encode(), "arm64-v8a", PACKAGE)
        for name, expected in helpers.items():
            adb("shell", "run-as", PACKAGE, "test", "-x", native_dir + "/" + name)
            assert adb("shell", "run-as", PACKAGE, "sha256sum", native_dir + "/" + name).split()[0] == expected
        receipt["nativeHelpers"] = helpers
        generated = list((ROOT / "apps/android/companion/build/generated").rglob("SafFolderSyncJourneyInstrumentedTest.kt"))
        assert len(generated) == 1
        source = ROOT / "apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"
        assert generated[0].read_bytes() == source.read_bytes()
        expected = run("sh", "-c", '. "$1"; derive_android_instrumentation_suite "$2"', "_",
                       str(ROOT / extra[1]), str(generated[0].parent))
        assert expected == TEST + "#nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant"
        (work / "expected.txt").write_text(expected + "\n")
        receipt["phase"] = "native-journey-running"
        save()
        started = time.monotonic()
        with (work / "instrumentation.log").open("wb") as log:
            result = subprocess.run([ADB, "-s", SERIAL, "shell", "am", "instrument", "-w", "-r",
                                     "-e", "class", TEST, PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"],
                                    stdout=log, stderr=subprocess.STDOUT)
        receipt["seconds"] = round(time.monotonic() - started, 3)
        receipt["instrumentationExitCode"] = result.returncode
        receipt["after"] = framework()
        receipt["sourceHashesUnchanged"] = all(digest(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        receipt["previousReceiptsUnchanged"] = all(digest(ROOT / p) == h for p, h in receipt["previousReceipts"].items())
        validation = run("sh", "-c", '. "$1"; validate_android_api37_result "$2" "$3"', "_",
                         str(ROOT / extra[1]), str(work / "instrumentation.log"), str(work / "expected.txt"))
        (work / "validation.log").write_text(validation + "\n")
        assert "COVALENT_JOURNEY_SURFACE=interlink-expressive-pair-first" in (work / "instrumentation.log").read_text()
        assert "COVALENT_COMPANION_SOURCE_SETUP_PEER_OFFLINE=true" in (work / "instrumentation.log").read_text()
        assert result.returncode == 0 and receipt["before"] == receipt["after"]
        assert receipt["sourceHashesUnchanged"] and receipt["previousReceiptsUnchanged"]
        receipt.update(passed=True, phase="companion-native-saf-journey-passed")
    except BaseException as failure:
        receipt.update(phase="failed-state-retained", failureClass=type(failure).__name__)
        save()
        raise
    finally:
        save()
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
