#!/usr/bin/env python3
"""One three-handoff attempt on the new, empty, owned API 37 emulator."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import time

ROOT = Path(__file__).resolve().parents[3]
BASE = Path(__file__).resolve().parent
SDK = Path.home() / "Library/Android/sdk"
ADB = str(SDK / "platform-tools/adb")
SERIAL = "emulator-5618"
AVD = "Interlink_Address_Soak_API37"
PACKAGE = "life.michaelwong.interlink"
CLASS = "life.michaelwong.covalent.node.SafFolderSyncJourneyInstrumentedTest"
METHOD = "nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(*args):
    return subprocess.run(args, capture_output=True, check=True, timeout=45).stdout.decode().strip()


def adb(*args):
    return run(ADB, "-s", SERIAL, *args)


def framework():
    value = {"bootId": adb("shell", "cat", "/proc/sys/kernel/random/boot_id")}
    for name in ("system_server", "surfaceflinger"):
        pid = adb("shell", "pidof", name)
        assert re.fullmatch(r"[1-9][0-9]*", pid)
        raw = adb("shell", "cat", "/proc/" + pid + "/stat")
        fields = raw.rsplit(")", 1)[1].split()
        assert fields[0] not in ("Z", "X", "x") and fields[19].isdecimal()
        value[name] = pid + ":" + fields[19]
    return value


def installed_digest(package):
    output = adb("shell", "pm", "path", package)
    assert re.fullmatch(r"package:/data/app/(?:~~[A-Za-z0-9_=-]+/)?" + re.escape(package) + r"-[A-Za-z0-9_=-]+/base\.apk", output)
    return adb("shell", "sha256sum", output[8:]).split()[0]


def main():
    out = BASE / "run"
    out.mkdir()
    receipt = {"format": 1, "serial": SERIAL, "avd": AVD, "syntheticOnly": True,
               "phase": "preflight", "passed": False, "physicalDeviceAccepted": False,
               "releaseReady": False, "dataCleared": False, "instrumentationRetried": False,
               "addressRoundsRequired": 3, "existingTransferTimeoutMillis": 120000}
    def save():
        (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    save()
    try:
        assert adb("get-state") == "device"
        assert adb("emu", "avd", "name").splitlines()[0] == AVD
        assert adb("shell", "getprop", "ro.kernel.qemu") == "1"
        assert adb("shell", "getprop", "ro.build.version.sdk") == "37"
        assert adb("shell", "getprop", "ro.product.cpu.abi") == "arm64-v8a"
        assert adb("shell", "getprop", "sys.boot_completed") == "1"
        assert not adb("shell", "cmd", "package", "list", "packages", "-U", PACKAGE)
        receipt["before"] = framework()
        receipt["systemFingerprint"] = adb("shell", "getprop", "ro.build.fingerprint")
        build = json.loads((BASE / "build-env-corrected/build.json").read_text())
        assert build["exitCode"] == 0 and build["sourcesUnchanged"]
        receipt["buildReceiptSha256"] = sha(BASE / "build-env-corrected/build.json")
        receipt["sourceHashes"] = build["sourceHashes"]
        receipt["previousEvidence"] = {
            p: sha(ROOT / p) for p in (
                "artifacts/evidence/android-acceptance-busy/native-picker-readiness/receipt.json",
                "artifacts/evidence/android-address-recovery/native-packaged-diagnostic/receipt.json",
                "artifacts/evidence/companion-native-journey/native-address-readiness/receipt.json",
            )
        }
        receipt["apkHashes"] = {p: v["sha256"] for p, v in build["artifacts"].items()}
        assert len(receipt["apkHashes"]) == 2
        os.environ["JAVA_HOME"] = "/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home"
        for index, (path, expected) in enumerate(receipt["apkHashes"].items()):
            apk = ROOT / path
            assert sha(apk) == expected
            tools = str(SDK / "build-tools/37.0.0")
            run(tools + "/zipalign", "-c", "-P", "16", "4", str(apk))
            assert "20a6923903ddb545a4458698925e2715e2d1b9df3d7dbc20d2fc927ea847018d" in run(tools + "/apksigner", "verify", "--print-certs", str(apk))
            value = adb("install", "--no-incremental", "--no-streaming", str(apk))
            (out / f"install-{index}.log").write_text(value + "\n")
            assert "Success" in value
            assert installed_digest(PACKAGE if index == 0 else PACKAGE + ".test") == expected
        apk = ROOT / next(iter(receipt["apkHashes"]))
        spec = importlib.util.spec_from_file_location("native_directory", ROOT / "scripts/android-native-library-directory.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        directory, helpers = module.resolve(apk, (adb("shell", "pm", "path", PACKAGE) + "\n").encode(), "arm64-v8a", PACKAGE)
        assert len(helpers) == 2
        for name, expected in helpers.items():
            adb("shell", "run-as", PACKAGE, "test", "-x", directory + "/" + name)
            assert adb("shell", "run-as", PACKAGE, "sha256sum", directory + "/" + name).split()[0] == expected
        receipt["nativeHelpers"] = helpers
        source = ROOT / "apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"
        generated = list((ROOT / "apps/android/companion/build/generated").rglob(source.name))
        assert len(generated) == 1 and generated[0].read_bytes() == source.read_bytes()
        adb("shell", "pm", "grant", PACKAGE, "android.permission.ACCESS_LOCAL_NETWORK")
        adb("shell", "settings", "put", "global", "device_provisioned", "1")
        adb("shell", "settings", "put", "secure", "user_setup_complete", "1")
        adb("shell", "input", "keyevent", "KEYCODE_WAKEUP")
        adb("shell", "wm", "dismiss-keyguard")
        assert framework() == receipt["before"]
        (out / "expected.txt").write_text(CLASS + "#" + METHOD + "\n")
        receipt["phase"] = "three-address-handoffs-running"
        save()
        started = time.monotonic()
        with (out / "instrumentation.log").open("xb") as log:
            result = subprocess.run([ADB, "-s", SERIAL, "shell", "am", "instrument", "-w", "-r", "-e", "class", CLASS + "#" + METHOD,
                "-e", "covalentAddressRecoveryOnly", "true", PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"],
                stdout=log, stderr=subprocess.STDOUT, timeout=720)
        receipt["seconds"] = round(time.monotonic() - started, 3)
        receipt["instrumentationExitCode"] = result.returncode
        validation = subprocess.run(["sh", "-c", '. "$1"; validate_android_api37_result "$2" "$3"', "_",
            str(ROOT / "scripts/android-instrumentation-result.sh"), str(out / "instrumentation.log"), str(out / "expected.txt")], capture_output=True)
        (out / "validation.log").write_bytes(validation.stdout + validation.stderr)
        receipt["resultValidationExitCode"] = validation.returncode
        receipt["after"] = framework()
        markers = [line for line in adb("logcat", "-d", "-v", "threadtime", "System.out:I", "*:S").splitlines() if "COVALENT_" in line]
        (out / "markers.log").write_text("\n".join(markers) + "\n")
        rounds = [line.split("COVALENT_ADDRESS_RECOVERY_ROUND ", 1)[1] for line in markers if "COVALENT_ADDRESS_RECOVERY_ROUND " in line]
        receipt["completedRounds"] = rounds
        receipt["sourceHashesUnchanged"] = all(sha(ROOT / p) == h for p, h in receipt["sourceHashes"].items())
        receipt["previousEvidenceUnchanged"] = all(sha(ROOT / p) == h for p, h in receipt["previousEvidence"].items())
        assert result.returncode == 0 and validation.returncode == 0 and receipt["after"] == receipt["before"]
        assert receipt["sourceHashesUnchanged"] and receipt["previousEvidenceUnchanged"]
        assert rounds == [f"round={n} generation={n + 1} phase=SUCCEEDED originalsUnchanged=true helpers=0" for n in (1, 2, 3)]
        assert any("COVALENT_ADDRESS_RECOVERY_ONLY rounds=3 complete=true" in line for line in markers)
        for index, expected in enumerate(receipt["apkHashes"].values()):
            assert installed_digest(PACKAGE if index == 0 else PACKAGE + ".test") == expected
        receipt.update(passed=True, phase="three-address-handoffs-passed")
    except BaseException as failure:
        receipt.update(phase="failed-state-retained", failureClass=type(failure).__name__)
        save()
        raise
    finally:
        save()
    print(json.dumps({k: v for k, v in receipt.items() if k not in ("sourceHashes", "previousEvidence")}))


if __name__ == "__main__":
    main()
