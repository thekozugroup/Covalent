#!/usr/bin/env python3
"""Exercise real permission revocation/recovery on an owned, unpaired debug AVD.

No app data is cleared. Only the named disposable QA emulators are accepted.
Receipts contain hashes and phase names, never key material or raw private files.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid
import xml.etree.ElementTree as ET
from interlink_framework_trace import FrameworkTrace


ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "life.michaelwong.interlink"
ACTIVITY = f"{PACKAGE}/life.michaelwong.covalent.InterlinkActivity"
PERMISSION = "android.permission.ACCESS_LOCAL_NETWORK"
OWNED_AVDS = {"Interlink_QA_API37": "emulator-5572", "Covalent_API_37": "emulator-5570",
              "Interlink_Unlock_API37": "emulator-5592"}
IDENTITIES = (
    "no_backup/covalent-node/identity.json",
    "no_backup/covalent-node/tls/identity.json",
    "no_backup/covalent-node-secrets/kek-envelope.v1",
)
SAFE_TEXTS = frozenset((
    "Get started", "Interlink", "Pair your Pi to continue", "Nearby devices access is off",
    "Allow access", "Open settings", "Permissions", "Nearby devices", "Allow", "Don't allow",
))
SAFE_PACKAGES = frozenset((PACKAGE, "com.android.settings", "com.android.permissioncontroller",
                           "com.google.android.permissioncontroller", "com.android.systemui"))


def command_diagnostic(command, result):
    """Retain error categories and hashes, never arbitrary adb output or arguments."""
    verbs = {"shell", "input", "tap", "keyevent", "pm", "grant", "revoke", "path", "am", "start",
             "force-stop", "run-as", "sha256sum", "getprop", "getconf", "pidof", "cat", "rm",
             "uiautomator", "dump", "dumpsys", "activity", "package", "install", "emu", "avd", "name"}
    label = " ".join(token if token in verbs else "<argument>" for token in command[:3])
    output = (result.stdout or "") + "\n" + (result.stderr or "")
    exceptions = ("java.lang.SecurityException", "java.lang.IllegalArgumentException",
                  "java.lang.IllegalStateException", "java.lang.NullPointerException",
                  "android.os.DeadObjectException", "android.os.RemoteException",
                  "android.os.ServiceSpecificException")
    signals = {
        "inject-events-permission": "INJECT_EVENTS",
        "permission-denial": "Permission Denial",
        "input-injection-failed": "Failed to inject",
        "missing-service": "Can't find service",
        "device-offline": "device offline",
        "device-not-found": "device not found",
        "unknown-command": "Unknown command",
        "security-exception": "Security exception",
        "broken-pipe": "Broken pipe",
        "timed-out": "timed out",
    }
    return {
        "command": label, "exitCode": result.returncode,
        "exceptionClasses": [name for name in exceptions if name in output],
        "signals": [name for name, marker in signals.items() if marker in output],
        "stdoutSha256": hashlib.sha256((result.stdout or "").encode()).hexdigest(),
        "stderrSha256": hashlib.sha256((result.stderr or "").encode()).hexdigest(),
    }


def ui_diagnostic(tree):
    """Allowlisted UI observations only; no arbitrary labels, account names or URLs."""
    if tree is None:
        return {"available": False}
    return {
        "available": True,
        "packages": sorted({n.get("package") for n in tree.iter("node") if n.get("package") in SAFE_PACKAGES}),
        "texts": sorted({n.get("text") for n in tree.iter("node") if n.get("text") in SAFE_TEXTS}),
        "runtimeAllowVisible": any(n.get("resource-id") in {
            "com.android.permissioncontroller:id/permission_allow_button",
            "com.google.android.permissioncontroller:id/permission_allow_button",
        } for n in tree.iter("node")),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--avd", required=True, choices=OWNED_AVDS)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    if OWNED_AVDS[args.avd] != args.serial:
        parser.error("serial does not match the owned QA AVD")
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
    if not sdk:
        parser.error("set ANDROID_HOME or ANDROID_SDK_ROOT")
    adb = str(Path(sdk) / "platform-tools/adb")
    apk = ROOT / "apps/android/companion/build/outputs/apk/debug/companion-debug.apk"
    if not apk.is_file():
        parser.error("build :companion:assembleDebug before booting the emulator")
    evidence = args.evidence_dir.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    receipt = {"avd": args.avd, "serial": args.serial, "phases": [], "result": "failed",
               "harnessSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    ui_path = f"/data/local/tmp/interlink-permission-{uuid.uuid4().hex}.xml"
    permission_mutated = False
    fixture_owned = False
    trace = None

    def run(*command, check=True, timeout=30):
        tap_input = command[:3] == ("shell", "input", "tap")
        if trace is not None and tap_input:trace.mark("input-tap-started")
        try:
            result = subprocess.run(
                [adb, "-s", args.serial, *command], capture_output=True, text=True, timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            if trace is not None and tap_input:trace.mark("input-tap-timeout")
            raise
        if trace is not None and tap_input:trace.mark("input-tap-returned",result.returncode)
        if check and result.returncode:
            diagnostic = command_diagnostic(command, result)
            receipt.setdefault("commandFailures", []).append(diagnostic)
            print("ADB failure: " + json.dumps(diagnostic, sort_keys=True), flush=True)
            raise RuntimeError(f"adb command failed: {diagnostic['command']} (exit {result.returncode})")
        return result.stdout.strip()

    def mark(phase):
        receipt["phases"].append(phase)
        if trace is not None:trace.mark(phase)
        print(f"PASS: {phase}", flush=True)

    def dump():
        # UIAutomator can return zero without a file while an Activity is
        # animating. Never reuse the preceding hierarchy as fresh evidence.
        run("shell", "rm", "-f", ui_path)
        try:
            output = run("shell", "uiautomator", "dump", ui_path, check=False, timeout=25)
        except subprocess.TimeoutExpired:
            return None
        if f"dumped to: {ui_path}" not in output:
            return None
        return ET.fromstring(run("shell", "cat", ui_path))

    def wait_ui(predicate, label, timeout=90):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            tree = dump()
            if tree is not None and predicate(tree):
                return tree
            time.sleep(1)
        raise RuntimeError(f"UI did not reach {label}")

    def has_text(tree, text):
        return any(n.get("text") == text for n in tree.iter("node"))

    def tap(tree, text=None, resource=None):
        matches = [n for n in tree.iter("node") if
                   (text is not None and n.get("text") == text) or
                   (resource is not None and n.get("resource-id") == resource)]
        if len(matches) != 1:
            raise RuntimeError("expected one visible UI action")
        bounds = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", matches[0].get("bounds", ""))
        if not bounds:
            raise RuntimeError("UI action has invalid bounds")
        x1, y1, x2, y2 = map(int, bounds.groups())
        if x2 <= x1 or y2 <= y1:
            raise RuntimeError("UI action is outside the viewport")
        receipt["lastAction"] = {"kind": "tap", "target": text if text in SAFE_TEXTS else "runtime permission allow",
                                 "sourceUi": ui_diagnostic(tree)}
        run("shell", "input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2))

    def launch():
        run("shell", "am", "start", "-W", "-n", ACTIVITY)

    def ready():
        # This native-API-backed state also refuses a real paired user's fixture.
        wait_ui(lambda t: has_text(t, "Pair your Pi to continue"), "unpaired native host ready")
        services = run("shell", "dumpsys", "activity", "services", PACKAGE)
        if "life.michaelwong.covalent.node.NodeProviderService" not in services or "isForeground=true" not in services:
            raise RuntimeError("native host does not have its foreground service")

    def hashes():
        values = {}
        for name in IDENTITIES:
            line = run("shell", "run-as", PACKAGE, "sha256sum", name)
            digest = line.split()[0]
            if not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise RuntimeError("invalid identity hash receipt")
            values[name] = digest
        return values

    def framework():
        boot = run("shell", "cat", "/proc/sys/kernel/random/boot_id")
        if not re.fullmatch(r"[0-9a-f-]{36}", boot):
            raise RuntimeError("invalid boot identity")
        values = {"bootId": boot}
        for name in ("system_server", "surfaceflinger"):
            pid = run("shell", "pidof", name)
            if not re.fullmatch(r"[1-9][0-9]*", pid):
                raise RuntimeError("expected one Android framework process")
            values[name] = int(pid)
        return values

    def revoke_and_open():
        nonlocal permission_mutated
        permission_mutated = True
        run("shell", "pm", "revoke", PACKAGE, PERMISSION)
        launch()  # Android kills the app on revocation; reopening is explicit.
        tree = wait_ui(lambda t: has_text(t, "Nearby devices access is off"), "permission recovery panel")
        if not has_text(tree, "Allow access") or not has_text(tree, "Open settings"):
            raise RuntimeError("permission recovery actions missing")
        return tree

    def grant_in_settings(tree):
        # Exercise the user route. A shell `pm grant` bypasses these controls
        # and failed on the hosted API37 guest after opening Settings.
        tap(tree, text="Permissions")
        tree = wait_ui(lambda t: has_text(t, "Nearby devices"), "Nearby devices in app permissions")
        tap(tree, text="Nearby devices")
        tree = wait_ui(lambda t: has_text(t, "Allow"), "Nearby devices permission options")
        tap(tree, text="Allow")
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            package = run("shell", "dumpsys", "package", PACKAGE)
            if re.search(r"(?m)^\s*" + re.escape(PERMISSION) + r": granted=true(?:,|$)", package):
                break
            time.sleep(0.5)
        else:
            raise RuntimeError("Settings did not grant Nearby devices access")
        # Return through the actual Settings back stack. Do not launch the app
        # or start its service: onResume must perform the recovery itself.
        deadline = time.monotonic() + 120
        back_actions = 0
        while time.monotonic() < deadline:
            tree = dump()
            if tree is None:
                time.sleep(1)
                continue
            if any(n.get("package") == PACKAGE for n in tree.iter("node")):
                return
            if not any(n.get("package") in {"com.android.settings", "com.android.permissioncontroller", "com.google.android.permissioncontroller"} for n in tree.iter("node")):
                raise RuntimeError("Unexpected app while returning from permission settings")
            if back_actions >= 4:
                raise RuntimeError("Settings back stack exceeded four observed pages")
            run("shell", "input", "keyevent", "KEYCODE_BACK")
            back_actions += 1
            time.sleep(0.5)
        raise RuntimeError("Settings did not return to the companion")

    try:
        if run("emu", "avd", "name").splitlines()[0] != args.avd:
            raise RuntimeError("emulator AVD name differs from the requested owned fixture")
        if run("shell", "getprop", "ro.kernel.qemu") != "1":
            raise RuntimeError("physical devices are not accepted")
        if run("shell", "getprop", "ro.build.version.sdk") != "37" or run("shell", "getprop", "sys.boot_completed") != "1":
            raise RuntimeError("fixture must be a booted API 37 emulator")
        receipt["frameworkBefore"] = framework()
        trace = FrameworkTrace(adb,args.serial,receipt["frameworkBefore"])
        trace.start()  # Read-only capture starts before APK install or permission mutations.
        if run("shell", "pm", "path", PACKAGE, check=False):
            run("shell", "run-as", PACKAGE, "true")  # Refuse an existing non-debug installation.
        run("install", "-r", str(apk), timeout=120)
        run("shell", "run-as", PACKAGE, "true")
        paths = run("shell", "pm", "path", PACKAGE).splitlines()
        if len(paths) != 1 or not paths[0].startswith("package:"):
            raise RuntimeError("expected a single installed debug APK")
        actual = run("shell", "sha256sum", paths[0].removeprefix("package:")).split()[0]
        with apk.open("rb") as source:
            expected = hashlib.file_digest(source, "sha256").hexdigest()
        if actual != expected:
            raise RuntimeError("installed companion differs from the built debug APK")
        fixture_owned = True
        receipt["apkSha256"] = expected
        receipt["pageSize"] = run("shell", "getconf", "PAGESIZE")
        launch()
        tree = wait_ui(lambda t: any(has_text(t, s) for s in (
            "Get started", "Nearby devices access is off", "Pair your Pi to continue")), "initial companion state")
        if has_text(tree, "Get started"):
            tap(tree, text="Get started")
            tree = wait_ui(lambda t: any(n.get("resource-id") == "com.android.permissioncontroller:id/permission_allow_button"
                                       for n in t.iter("node")) or has_text(t, "Pair your Pi to continue"), "initial permission dialog")
            if not has_text(tree, "Pair your Pi to continue"):
                tap(tree, resource="com.android.permissioncontroller:id/permission_allow_button")
        elif has_text(tree, "Nearby devices access is off"):
            # Restore a previously interrupted test fixture, without discarding its identity.
            run("shell", "pm", "grant", PACKAGE, PERMISSION)
            run("shell", "am", "force-stop", PACKAGE)
            launch()
        ready()
        before = hashes()
        receipt["identitySha256Before"] = before
        mark("exact debug APK and native host ready")

        tree = revoke_and_open()
        mark("revoked permission shows actionable recovery")
        tap(tree, text="Open settings")
        tree = wait_ui(lambda t: has_text(t, "Interlink") and has_text(t, "Permissions") and
                any(n.get("package") == "com.android.settings" for n in t.iter("node")), "Interlink app settings")
        grant_in_settings(tree)
        ready()
        if hashes() != before:
            raise RuntimeError("saved identity changed after Settings recovery")
        mark("Settings return restarts native host and preserves identities")

        tree = revoke_and_open()
        tap(tree, text="Allow access")
        tree = wait_ui(lambda t: any(n.get("resource-id") == "com.android.permissioncontroller:id/permission_allow_button"
                                   for n in t.iter("node")), "runtime permission dialog")
        tap(tree, resource="com.android.permissioncontroller:id/permission_allow_button")
        ready()
        after = hashes()
        if after != before:
            raise RuntimeError("saved identity changed after runtime recovery")
        receipt["identitySha256After"] = after
        permission_mutated = False  # Both real grant routes already restored it.
        mark("runtime grant restarts native host and preserves identities")
        receipt["frameworkAfter"] = framework()
        if receipt["frameworkAfter"] != receipt["frameworkBefore"]:
            raise RuntimeError("Android framework restarted during permission recovery")
        mark("Android framework remained stable through both recovery routes")
        receipt["result"] = "passed"
    except (RuntimeError, subprocess.SubprocessError, OSError, ET.ParseError) as error:
        receipt["failureClass"] = type(error).__name__
        if trace is not None:trace.mark("failure-observation-started")
        # Observe the existing guest before cleanup. A failing input command
        # might have delivered the tap; keep that distinction without accepting
        # its nonzero exit, replaying input, or restarting the emulator.
        if fixture_owned:
            for name, observe in (("failureUi", lambda: ui_diagnostic(dump())),
                                  ("frameworkAtFailure", framework), ("identitySha256AtFailure", hashes)):
                try:
                    receipt[name] = observe()
                except (RuntimeError, subprocess.SubprocessError, OSError, ET.ParseError) as observation_error:
                    receipt[name] = {"observationFailureClass": type(observation_error).__name__}
        if trace is not None:trace.mark("failure-observation-completed")
        raise
    finally:
        cleanup_failures = []
        if trace is not None:trace.mark("cleanup-started")
        if fixture_owned:
            actions = [("remove owned UI dump", ("shell", "rm", "-f", ui_path))]
            if permission_mutated:
                actions.insert(0, ("restore QA permission", ("shell", "pm", "grant", PACKAGE, PERMISSION)))
            for name, command in actions:
                try:
                    run(*command)
                except (RuntimeError, subprocess.SubprocessError, OSError) as cleanup_error:
                    cleanup_failures.append({"action": name, "failureClass": type(cleanup_error).__name__})
        if trace is not None:
            try:
                trace.mark("cleanup-completed")
                receipt["frameworkTrace"] = trace.stop()
                if receipt["frameworkTrace"]["sourceChanged"] or not receipt["frameworkTrace"]["streamReady"] or not receipt["frameworkTrace"]["streamLiveBeforeStop"] or receipt["frameworkTrace"]["readerAliveAfterStop"] or receipt["frameworkTrace"]["failureClass"]:
                    cleanup_failures.append({"action":"retain continuous framework trace","failureClass":"TraceUnavailable"})
            except (RuntimeError,subprocess.SubprocessError,OSError) as trace_error:
                cleanup_failures.append({"action":"stop owned framework trace","failureClass":type(trace_error).__name__})
        cleanup_invalidated_pass = bool(cleanup_failures) and receipt["result"] == "passed"
        if cleanup_failures:
            receipt["cleanupFailures"] = cleanup_failures
            receipt["result"] = "failed"
        (evidence / "permission-recovery.json").write_text(json.dumps(receipt, indent=2) + "\n")
        if cleanup_invalidated_pass:
            raise RuntimeError("QA fixture cleanup failed; failure receipt retained")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.SubprocessError, OSError, ET.ParseError) as error:
        # No raw adb stderr, private file content or environment is included.
        print(f"Interlink permission recovery failed: {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
