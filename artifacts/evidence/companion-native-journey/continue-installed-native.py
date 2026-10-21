#!/usr/bin/env python3
"""Explicit first instrumentation after the retained installed-update UID refusal.

No installation, data clearing, reboot or previous instrumentation is repeated.
"""
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import time

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("native_fixture", HERE / "run-native.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
ROOT, PACKAGE, TEST = fixture.ROOT, fixture.PACKAGE, fixture.TEST
adb, run, digest, framework = fixture.adb, fixture.run, fixture.digest, fixture.framework


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
    previous = HERE / "native"
    original = previous / "receipt.json"
    old = json.loads(original.read_text())
    assert old["phase"] == "failed-state-retained" and old["failureClass"] == "AssertionError"
    assert old["passed"] is False and not (previous / "instrumentation.log").exists()
    assert all("Success" in (previous / name).read_text() for name in ("install.log", "install-test.log"))
    work = HERE / "native-after-installed-update"
    work.mkdir()
    receipt = {"format": 1, "serial": fixture.SERIAL, "avd": "Interlink_Address_API37",
               "syntheticOnly": True, "passed": False, "phase": "continuation-preflight",
               "installedUpdateReceiptSha256": digest(original), "originalRunnerSha256": digest(HERE / "run-native.py"),
               "appReinstalled": False, "testReinstalled": False, "dataCleared": False,
               "instrumentationRetried": False, "physicalDeviceAccepted": False, "releaseReady": False}
    def save():
        (work / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    save()
    try:
        assert adb("get-state") == "device"
        assert adb("emu", "avd", "name").splitlines()[0] == "Interlink_Address_API37"
        assert adb("shell", "getprop", "ro.kernel.qemu") == "1"
        assert adb("shell", "getprop", "ro.build.version.sdk") == "37"
        assert framework() == old["before"] and exact_uid() == old["beforeUid"]
        assert installed_digest(PACKAGE) == old["apkSha256"]
        assert installed_digest(PACKAGE + ".test") == old["testApkSha256"]
        assert "ActiveInstrumentation{" not in adb("shell", "dumpsys", "activity", "instrumentation")
        for name in ("before", "sourceHashes", "previousReceipts", "apkSha256", "testApkSha256"):
            receipt[name] = old[name]
        assert all(digest(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        assert all(digest(ROOT / p) == h for p, h in receipt["previousReceipts"].items())
        helper_path = ROOT / "scripts/android-native-library-directory.py"
        helper_spec = importlib.util.spec_from_file_location("native_directory", helper_path)
        helper = importlib.util.module_from_spec(helper_spec)
        helper_spec.loader.exec_module(helper)
        apk = ROOT / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
        assert digest(apk) == old["apkSha256"]
        directory, hashes = helper.resolve(apk, (adb("shell", "pm", "path", PACKAGE) + "\n").encode(), "arm64-v8a", PACKAGE)
        for name, expected in hashes.items():
            adb("shell", "run-as", PACKAGE, "test", "-x", directory + "/" + name)
            assert adb("shell", "run-as", PACKAGE, "sha256sum", directory + "/" + name).split()[0] == expected
        receipt["nativeHelpers"] = hashes
        generated = list((ROOT / "apps/android/companion/build/generated").rglob("SafFolderSyncJourneyInstrumentedTest.kt"))
        assert len(generated) == 1
        source = ROOT / "apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"
        assert generated[0].read_bytes() == source.read_bytes()
        validator = ROOT / "scripts/android-instrumentation-result.sh"
        expected = run("sh", "-c", '. "$1"; derive_android_instrumentation_suite "$2"', "_", str(validator), str(generated[0].parent))
        assert expected == TEST + "#nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant"
        (work / "expected.txt").write_text(expected + "\n")
        receipt["phase"] = "native-journey-running"
        save()
        start = time.monotonic()
        with (work / "instrumentation.log").open("wb") as output:
            result = subprocess.run([fixture.ADB, "-s", fixture.SERIAL, "shell", "am", "instrument", "-w", "-r",
                                     "-e", "class", TEST, PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"],
                                    stdout=output, stderr=subprocess.STDOUT)
        receipt["seconds"] = round(time.monotonic() - start, 3)
        receipt["instrumentationExitCode"] = result.returncode
        receipt["after"] = framework()
        receipt["sourceHashesUnchanged"] = all(digest(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        receipt["previousReceiptsUnchanged"] = all(digest(ROOT / p) == h for p, h in receipt["previousReceipts"].items())
        receipt["installedUpdateReceiptUnchanged"] = digest(original) == receipt["installedUpdateReceiptSha256"]
        text = run("sh", "-c", '. "$1"; validate_android_api37_result "$2" "$3"', "_", str(validator), str(work / "instrumentation.log"), str(work / "expected.txt"))
        (work / "validation.log").write_text(text + "\n")
        log = (work / "instrumentation.log").read_text()
        assert "COVALENT_JOURNEY_SURFACE=interlink-expressive-pair-first" in log
        assert "COVALENT_COMPANION_SOURCE_SETUP_PEER_OFFLINE=true" in log
        assert result.returncode == 0 and receipt["after"] == receipt["before"]
        assert receipt["sourceHashesUnchanged"] and receipt["previousReceiptsUnchanged"] and receipt["installedUpdateReceiptUnchanged"]
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
