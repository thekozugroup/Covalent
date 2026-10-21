#!/usr/bin/env python3
"""One real reboot of the fresh, synthetic Interlink_Unlock_API37 fixture only.

Requires a passing permission receipt from the same APK and boot. Never opens
the app, sends a boot broadcast, starts a service, grants access or clears data.
An uncertain reboot result is retained; it is never retried.
"""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import subprocess
import time
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "life.michaelwong.interlink"
SERIAL = "emulator-5592"
AVD = "Interlink_Unlock_API37"
IDENTITIES = ("no_backup/covalent-node/identity.json", "no_backup/covalent-node/tls/identity.json",
              "no_backup/covalent-node-secrets/kek-envelope.v1")
SOURCES = ("scripts/interlink-api37-reboot-test.py", "scripts/interlink-api37-permission-test.py",
           "apps/android/app/src/main/java/life/michaelwong/covalent/node/DirectBoot.kt",
           "apps/android/app/src/main/java/life/michaelwong/covalent/node/UserUnlockGate.kt",
           "apps/android/app/src/main/java/life/michaelwong/covalent/node/NodeRestartReceiver.kt",
           "apps/android/companion/src/main/java/life/michaelwong/covalent/InterlinkApplication.kt")


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


class Refused(RuntimeError):
    """Only fixed source-defined reasons may enter the receipt."""


def require(ok, message):
    if not ok:
        raise Refused(message)


def remove_owned_forwards(run, forwards):
    """Reboot can remove a forward before cleanup; verify absence or exact ownership."""
    def mappings():
        rows = [line.split() for line in run("forward", "--list").splitlines() if line.strip()]
        require(all(len(row) == 3 for row in rows), "ADB forward inventory is incomplete")
        return rows

    results = []
    for remote, local in forwards.items():
        endpoint = "tcp:" + str(local)
        try:
            matches = [row for row in mappings() if row[1] == endpoint]
            if not matches:
                results.append("already-absent")
                continue
            if matches != [[SERIAL, endpoint, "tcp:" + str(remote)]]:
                results.append("mapping-changed")
                continue
            failed = False
            try:
                run("forward", "--remove", endpoint)
            except (RuntimeError, OSError, subprocess.SubprocessError):
                failed = True
            if any(row[1] == endpoint for row in mappings()):
                results.append("still-present")
            else:
                results.append("absent-after-remove-error" if failed else "removed-readback-absent")
        except (RuntimeError, OSError, subprocess.SubprocessError):
            results.append("inventory-unavailable")
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--permission-receipt", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
    if not sdk:
        parser.error("set ANDROID_HOME or ANDROID_SDK_ROOT")
    proof = json.loads(args.permission_receipt.read_text())
    require(proof.get("result") == "passed" and proof.get("serial") == SERIAL and proof.get("avd") == AVD,
            "A passing permission journey on the exact fresh fixture is required")
    require(proof.get("harnessSha256") == digest(ROOT / "scripts/interlink-api37-permission-test.py"),
            "Permission evidence belongs to another harness")
    source_hashes = {name: digest(ROOT / name) for name in SOURCES}
    apk = ROOT / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
    require(digest(apk) == proof.get("apkSha256"), "Built APK differs from the permission journey")
    # Refuse every existing directory, including failed or uncertain preceding runs.
    evidence = args.evidence_dir.resolve()
    evidence.mkdir(mode=0o700, parents=True, exist_ok=False)
    receipt = {"result": "failed", "avd": AVD, "serial": SERIAL, "sourceSha256": source_hashes,
               "permissionReceiptSha256": digest(args.permission_receipt), "apkSha256": digest(apk),
               "phases": [], "rebootAttempts": 0, "activityLaunches": 0,
               "serviceStartCommands": 0, "realLockedUserTested": False,
               "physicalDeviceTested": False}
    forwards = {}
    core_completed = False

    def checkpoint(phase):
        receipt["phases"].append(phase)
        temporary = evidence / "reboot.next"
        with temporary.open("w") as out:
            json.dump(receipt, out, indent=2, sort_keys=True)
            out.write("\n")
            out.flush()
            os.fsync(out.fileno())
        os.replace(temporary, evidence / "reboot.json")
        descriptor = os.open(evidence, os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        print(phase, flush=True)

    def run(*command, check=True):
        result = subprocess.run([str(Path(sdk) / "platform-tools/adb"), "-s", SERIAL, *command],
                                capture_output=True, text=True, timeout=15)
        if check:
            require(result.returncode == 0, "ADB command did not finish successfully")
        return result.stdout.strip() if result.returncode == 0 else ""

    def boot():
        value = run("shell", "cat", "/proc/sys/kernel/random/boot_id")
        require(re.fullmatch(r"[0-9a-f-]{36}", value), "Boot identity unavailable")
        return value

    def pid(name):
        value = run("shell", "pidof", name)
        require(re.fullmatch(r"[1-9][0-9]*", value), "Expected one stable process")
        return int(value)

    def identities():
        result = {}
        for name in IDENTITIES:
            value = run("shell", "run-as", PACKAGE, "sha256sum", name).split()[0]
            require(re.fullmatch(r"[0-9a-f]{64}", value), "Identity digest unavailable")
            result[name] = value
        return result

    def uid():
        value = run("shell", "cmd", "package", "list", "packages", "-U", PACKAGE)
        match = re.fullmatch(r"package:life\.michaelwong\.interlink uid:([0-9]+)", value)
        require(match is not None, "App UID unavailable")
        return int(match[1])

    def foreground():
        value = run("shell", "dumpsys", "activity", "services", PACKAGE)
        for block in re.split(r"(?m)^  \* ServiceRecord\{", value)[1:]:
            if (PACKAGE + "/life.michaelwong.covalent.node.NodeProviderService" in block.splitlines()[0]
                    and re.search(r"\bisForeground=true\b", block)):
                return True
        return False

    def native_status():
        # Read only the loopback endpoint from this new synthetic fixture. The
        # encrypted envelope and all other preferences are discarded in memory.
        data = run("shell", "run-as", PACKAGE, "head", "-c", "65537",
                   "shared_prefs/covalent_embedded_node_credentials.xml")
        require(len(data.encode()) <= 65536, "Credential preferences exceed the bounded fixture size")
        tree = ET.fromstring(data)
        endpoints = [n.text for n in tree if n.tag == "string" and n.get("name") == "base_url"]
        require(len(endpoints) == 1, "Expected one local native endpoint")
        match = re.fullmatch(r"http://127\.0\.0\.1:([0-9]{1,5})", endpoints[0] or "")
        require(match is not None and 0 < int(match[1]) < 65536, "Refused a non-loopback native endpoint")
        port = int(match[1])
        if port not in forwards:
            local = run("forward", "tcp:0", "tcp:" + str(port))
            require(re.fullmatch(r"[0-9]{1,5}", local) and 0 < int(local) < 65536, "Forward port unavailable")
            forwards[port] = int(local)
        connection = http.client.HTTPConnection("127.0.0.1", forwards[port], timeout=2)
        try:
            connection.request("GET", "/api/v1/status")
            response = connection.getresponse()
            data = response.read(16385)
            require(len(data) <= 16384, "Native status exceeds its size limit")
            if response.status != 200:
                return None
            status = json.loads(data)
            if not isinstance(status, dict) or status.get("state") != "ready" or type(status.get("protocolVersion")) is not int:
                return None
            return {"state": "ready", "protocolVersion": status["protocolVersion"]}
        except (OSError, http.client.HTTPException):
            return None
        finally:
            connection.close()

    def installed_digest():
        value = run("shell", "pm", "path", PACKAGE)
        require(value.startswith("package:") and "\n" not in value, "Expected one installed APK")
        return run("shell", "sha256sum", value.removeprefix("package:")).split()[0]

    try:
        require(run("emu", "avd", "name").splitlines()[0] == AVD, "Unrelated emulator")
        require(run("shell", "getprop", "ro.kernel.qemu") == "1", "Physical device refused")
        require(run("shell", "getprop", "ro.build.version.sdk") == "37", "Expected API37")
        require(run("shell", "am", "get-current-user") == "0", "Expected the synthetic primary user")
        require(run("shell", "getprop", "sys.boot_completed") == "1", "Boot incomplete")
        old_boot = boot()
        require(old_boot == proof.get("frameworkAfter", {}).get("bootId"), "Permission proof is from another boot")
        require(installed_digest() == receipt["apkSha256"], "Installed APK differs")
        before = identities()
        require(before == proof.get("identitySha256After"), "Identity differs from the permission journey")
        require(foreground(), "Opted-in host is not running before reboot")
        status_before = native_status()
        require(status_before is not None, "Native host is not ready before reboot")
        receipt.update(bootBefore=old_boot, uidBefore=uid(), identitySha256Before=before,
                       nativeStatusBefore=status_before)
        checkpoint("admitted-exact-opted-in-debug-fixture")
        receipt["rebootAttempts"] = 1
        checkpoint("reboot-issued-unconfirmed")
        run("reboot")  # Exactly one attempt. An error or timeout does not replay it.
        checkpoint("reboot-command-returned")
        deadline = time.monotonic() + 180
        framework = None
        new_boot = None
        stable = None
        stable_since = None
        while time.monotonic() < deadline:
            if run("get-state", check=False) != "device":
                require(stable is None, "Transport lost after host startup")
                time.sleep(1)
                continue
            current_boot = boot()
            if new_boot is None and current_boot != old_boot:
                new_boot = current_boot
            if new_boot is not None:
                require(current_boot == new_boot, "Another boot observed; no reboot replay allowed")
            if current_boot == old_boot or run("shell", "getprop", "sys.boot_completed") != "1":
                require(framework is None, "Boot changed after readiness")
                time.sleep(1)
                continue
            observed = {"bootId": current_boot, "system_server": pid("system_server"),
                        "surfaceflinger": pid("surfaceflinger")}
            if framework is None:
                framework = observed.copy()
                receipt["frameworkAfter"] = framework
                checkpoint("new-boot-observed")
            require(observed == framework, "Framework restarted during recovery")
            if foreground() and native_status() == status_before:
                activities = run("shell", "dumpsys", "activity", "activities")
                resumed = [line for line in activities.splitlines()
                           if "topResumedActivity=" in line or "mResumedActivity:" in line]
                require(bool(resumed), "Resumed activity observation unavailable")
                require(not any(PACKAGE + "/" in line for line in resumed),
                        "App activity resumed after reboot; background startup not established")
                observed["appPid"] = pid(PACKAGE)
                if stable is None:
                    stable, stable_since = observed.copy(), time.monotonic()
                    checkpoint("foreground-host-started-without-opening-app")
                require(observed == stable, "Host restarted during stability observation")
                if time.monotonic() - stable_since >= 20:
                    receipt["stableSeconds"] = round(time.monotonic() - stable_since, 3)
                    receipt["nativeStatusAfter"] = status_before
                    receipt["appActivityResumedAfterBoot"] = False
                    break
            else:
                require(stable is None, "Host disappeared during stability observation")
            time.sleep(1)
        else:
            raise Refused("Boot recovery deadline expired; observe this same fixture before further action")
        after = identities()
        require(after == before and uid() == receipt["uidBefore"], "Identity or app UID changed")
        require(installed_digest() == receipt["apkSha256"], "Installed APK changed")
        require({name: digest(ROOT / name) for name in SOURCES} == source_hashes, "Sources changed during the journey")
        receipt.update(identitySha256After=after, uidAfter=uid(), nativeRebootVerified=True)
        checkpoint("real-reboot-preserves-identity-and-autostarts-host")
        core_completed = True
    except (RuntimeError, OSError, ValueError, IndexError, ET.ParseError, subprocess.SubprocessError) as failure:
        receipt["failureClass"] = type(failure).__name__
        if isinstance(failure, Refused):
            receipt["failureReason"] = str(failure)
        checkpoint("failed-no-reboot-replay")
        raise
    finally:
        cleanup = remove_owned_forwards(run, forwards)
        removed = [state in {"already-absent", "removed-readback-absent", "absent-after-remove-error"}
                   for state in cleanup]
        receipt["ownedForwardCleanup"] = cleanup
        receipt["ownedForwardsRemoved"] = removed
        if not all(removed):
            receipt["result"] = "failed"
            checkpoint("owned-forward-cleanup-failed")
            raise Refused("Owned ADB forward cleanup incomplete")
        if core_completed:
            receipt["result"] = "passed"
        checkpoint("owned-forwards-removed")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, IndexError, ET.ParseError, subprocess.SubprocessError) as failure:
        print("Reboot check failed: " + type(failure).__name__, flush=True)
        if isinstance(failure, Refused):
            print(str(failure), flush=True)
        raise SystemExit(1)
