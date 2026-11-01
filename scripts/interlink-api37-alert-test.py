#!/usr/bin/env python3
"""Run normal-package drive alerts on an owned unpaired API37 QA guest only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import uuid
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "life.michaelwong.interlink"
OWNED_AVDS = {"Covalent_API_37": "emulator-5570", "Interlink_Unlock_API37": "emulator-5592"}
IDENTITIES = ("no_backup/covalent-node/identity.json", "no_backup/covalent-node/tls/identity.json",
              "no_backup/covalent-node-secrets/kek-envelope.v1")
CLASSES = ("InterlinkImportAlertsInstrumentedTest", "InterlinkDashboardInstrumentedTest",
           "InterlinkImportsInstrumentedTest", "InterlinkNotificationInstrumentedTest", "ui.InterlinkLayoutInstrumentedTest")


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def admit(proof, serial, avd, apk_sha, boot):
    require(OWNED_AVDS.get(avd) == serial, "Unowned QA serial/name")
    require(proof.get("result") == "passed" and proof.get("serial") == serial and proof.get("avd") == avd,
            "A passing owned startup receipt is required")
    require(proof.get("apkSha256") == apk_sha and proof.get("frameworkAfter", {}).get("bootId") == boot,
            "Startup proof belongs to another APK or boot")
    require(set(proof.get("identitySha256After", {})) == set(IDENTITIES), "Incomplete startup identity proof")


def expected_tests():
    expected = set()
    for name in CLASSES:
        path = ROOT / "apps/android/companion/src/androidTest/java/life/michaelwong/covalent" / (name.replace(".", "/") + ".kt")
        source = path.read_text()
        methods = re.findall(r"@Test\s+fun\s+(\w+)\s*\(", source)
        require(methods and len(methods) == len(set(methods)) == len(re.findall(r"@Test\b", source)) and "@Ignore" not in source,
                "Incomplete native alert test expectation")
        expected.update("life.michaelwong.covalent." + name + "#" + method for method in methods)
    return expected


def instrument_passed(output):
    expected = expected_tests()
    passed = set(); active = {}
    for line in output.splitlines():
        match = re.fullmatch(r"INSTRUMENTATION_STATUS: (class|test)=(.+)", line)
        if match:
            active[match[1]] = match[2]
        elif line.startswith("INSTRUMENTATION_STATUS_CODE: "):
            code = line.removeprefix("INSTRUMENTATION_STATUS_CODE: ")
            if code not in ("0", "1") or set(active) != {"class", "test"}:
                return False
            if code == "0":
                key = active["class"] + "#" + active["test"]
                if key in passed:
                    return False
                passed.add(key)
            active = {}
    return not active and passed == expected and re.findall(r"^OK \((\d+) tests\)$", output, re.M) == [str(len(expected))] and \
        re.findall(r"^INSTRUMENTATION_CODE: (-?\d+)$", output, re.M) == ["-1"] and \
        not re.search(r"^INSTRUMENTATION_STATUS_CODE: -|INSTRUMENTATION_FAILED|FAILURES!!!|shortMsg=|Process crashed", output, re.M)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--avd", required=True, choices=OWNED_AVDS)
    parser.add_argument("--startup-receipt", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    require(OWNED_AVDS[args.avd] == args.serial, "Unowned QA serial/name")
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
    require(bool(sdk), "Set the pinned Android SDK")
    adb = str(Path(sdk) / "platform-tools/adb")
    apk = ROOT / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
    test = ROOT / "apps/android/companion/build/outputs/apk/androidTest/debug/companion-debug-androidTest.apk"
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    require(apk.is_file() and test.is_file(), "Build both normal companion APKs")
    proof = json.loads(args.startup_receipt.read_text())
    args.evidence_dir.mkdir(parents=True, exist_ok=False)
    report = {"result": "failed", "serial": args.serial, "avd": args.avd, "phases": [],
              "harnessSha256": sha(Path(__file__)), "apkSha256": sha(apk), "testApkSha256": sha(test),
              "startupReceiptSha256": sha(args.startup_receipt), "syntheticDriveData": True,
              "physicalDeviceOrCloudTested": False}
    remote = "/data/local/tmp/interlink-alerts-" + uuid.uuid4().hex + ".xml"
    report["ownedUiDump"] = remote
    (args.evidence_dir / "alerts.json").write_text(json.dumps(report, indent=2) + "\n")
    owned = False

    def run(*command, timeout=30):
        result = subprocess.run([adb, "-s", args.serial, *command], capture_output=True, timeout=timeout)
        require(result.returncode == 0 and len(result.stdout) <= 131072, "Owned QA command failed or exceeded its bound")
        return result.stdout.decode().strip()

    def mark(phase):
        report["phases"].append(phase)
        print("PASS: " + phase, flush=True)

    def identities():
        return {n: run("shell", "run-as", PACKAGE, "sha256sum", n).split()[0] for n in IDENTITIES}

    def framework():
        return {"bootId": run("shell", "cat", "/proc/sys/kernel/random/boot_id"),
                **{n: int(run("shell", "pidof", n)) for n in ("system_server", "surfaceflinger")}}

    def tree():
        run("shell", "rm", "-f", remote)
        output = run("shell", "uiautomator", "dump", remote)
        if "dumped to: " + remote not in output:
            return None
        return ET.fromstring(run("shell", "cat", remote))

    def wait_ui(predicate):
        until = time.monotonic() + 60
        while time.monotonic() < until:
            value = tree()
            if value is not None and predicate(value):
                return value
            time.sleep(.5)
        raise RuntimeError("Owned permission UI did not reach its expected state")

    def has_text(value, text):
        return any(n.get("text") == text for n in value.iter("node"))

    def tap(value, predicate):
        nodes = [n for n in value.iter("node") if predicate(n)]
        require(len(nodes) == 1, "Expected one observed permission action")
        bounds = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", nodes[0].get("bounds", ""))
        require(bounds is not None, "Invalid action bounds")
        x, y, right, bottom = map(int, bounds.groups())
        require(right > x and bottom > y, "Unreachable permission action")
        run("shell", "input", "tap", str((x + right) // 2), str((y + bottom) // 2))

    try:
        require(run("emu", "avd", "name").splitlines()[0] == args.avd and run("shell", "getprop", "ro.kernel.qemu") == "1",
                "Physical or unrelated devices refused")
        require(run("shell", "getprop", "ro.build.version.sdk") == "37" and run("shell", "getprop", "sys.boot_completed") == "1",
                "Booted API37 required")
        before = framework()
        admit(proof, args.serial, args.avd, sha(apk), before["bootId"])
        require(before == proof["frameworkAfter"], "Framework differs from startup proof")
        require(identities() == proof["identitySha256After"], "Startup identity changed")
        installed = run("shell", "pm", "path", PACKAGE)
        require(installed.startswith("package:") and "\n" not in installed, "Expected one installed normal APK")
        require(run("shell", "sha256sum", installed.removeprefix("package:")).split()[0] == sha(apk), "Installed APK differs")
        owned = True
        report["frameworkBefore"] = before
        mark("admitted exact normal package, native startup, owned guest and identities")
        package = run("shell", "dumpsys", "package", PACKAGE)
        granted = bool(re.search(r"android.permission.POST_NOTIFICATIONS: granted=true(?:,|$)", package, re.M))
        report["notificationPermissionAlreadyGranted"] = granted
        if not granted:
            run("shell", "am", "start", "-W", "-n", PACKAGE + "/life.michaelwong.covalent.InterlinkPreviewActivity",
                "--es", "previewMode", "alert-settings")
            value = wait_ui(lambda v: has_text(v, "Allow import alerts"))
            tap(value, lambda n: n.get("text") == "Allow import alerts")
            ids = {p + ":id/permission_allow_button" for p in ("com.android.permissioncontroller", "com.google.android.permissioncontroller")}
            value = wait_ui(lambda v: any(n.get("resource-id") in ids for n in v.iter("node")))
            tap(value, lambda n: n.get("resource-id") in ids)
            wait_ui(lambda v: has_text(v, "Import alerts are allowed"))
            mark("actual notification consent updates the production control")
        run("shell", "input", "keyevent", "KEYCODE_HOME")
        run("install", "-r", str(test), timeout=120)
        test_path = run("shell", "pm", "path", PACKAGE + ".test").removeprefix("package:")
        require("\n" not in test_path and run("shell", "sha256sum", test_path).split()[0] == sha(test), "Installed test APK differs")
        output = run("shell", "am", "instrument", "-w", "-r", "-e", "class",
                     ",".join("life.michaelwong.covalent." + c for c in CLASSES),
                     PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner", timeout=300)
        (args.evidence_dir / "instrumentation.log").write_text(output + "\n")
        require(instrument_passed(output), "Native alert/component instrumentation failed; original log retained")
        mark(f"all{len(expected_tests())} native owner, background, notification and component checks passed by name")
        require(framework() == before and identities() == proof["identitySha256After"], "Guest framework or identities changed")
        report["frameworkAfter"] = before
        report["identitySha256After"] = proof["identitySha256After"]
        report["result"] = "passed"
    except Exception as error:
        report["failureClass"] = type(error).__name__
        raise
    finally:
        if owned:
            try:
                run("shell", "rm", "-f", remote)
            except (RuntimeError, OSError, subprocess.SubprocessError):
                report["result"] = "failed"
                report["cleanupFailure"] = "owned UI dump removal"
        (args.evidence_dir / "alerts.json").write_text(json.dumps(report, indent=2) + "\n")
        require(report["result"] == "passed", "Alert journey did not pass; receipt retained")


if __name__ == "__main__":
    main()
