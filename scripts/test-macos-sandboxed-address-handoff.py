#!/usr/bin/env python3
"""Exercise unchanged packaged workers through a real sandboxed native parent."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parent.parent
PARENT_SOURCE = REPO / "packaging/macos-qa/sandboxed-worker-parent.m"
PARENT_ENTITLEMENTS = REPO / "packaging/macos-qa/Parent.entitlements"
INHERITED_ENTITLEMENTS = REPO / "apps/apple/Config/CovalentNode.entitlements"
TEST_NAME = "manual_link_survives_both_endpoint_moves_and_retains_pairing"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def inventory(root):
    result = {}
    for path in sorted(root.rglob("*")):
        name = path.relative_to(root).as_posix()
        if path.is_symlink():
            result[name] = {"symlink": os.readlink(path)}
        elif path.is_file():
            result[name] = {"sha256": digest(path), "mode": path.stat().st_mode & 0o777}
    return result


def test_executable(messages):
    candidates = []
    for line in messages.splitlines():
        record = json.loads(line)
        if not isinstance(record, dict):
            raise ValueError("cargo output record is not an object")
        target = record.get("target", {})
        if not isinstance(target, dict):
            raise ValueError("cargo target record is not an object")
        if (record.get("reason") == "compiler-artifact"
                and target.get("name") == "one_way_links"
                and target.get("kind") == ["test"]
                and record.get("executable")):
            if not isinstance(record["executable"], str):
                raise ValueError("cargo executable path is not text")
            candidates.append(Path(record["executable"]))
    if len(candidates) != 1:
        raise ValueError("cargo must report exactly one one_way_links test executable")
    binary = candidates[0]
    if not binary.is_absolute() or binary.is_symlink() or not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError("cargo test executable is missing or unsafe")
    return binary


def run_logged(command, output, label, receipt, timeout=120, environment=None):
    stdout = output / f"{label}.stdout.log"
    stderr = output / f"{label}.stderr.log"
    started = time.monotonic()
    timed_out = False
    with stdout.open("xb") as out, stderr.open("xb") as err:
        process = subprocess.Popen(command, cwd=REPO, env=environment, stdout=out, stderr=err,
                                   start_new_session=True)
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGTERM)
            try:
                code = process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                code = process.wait()
    receipt["commands"].append({"label": label, "exitCode": code, "timedOut": timed_out,
                                "seconds": round(time.monotonic() - started, 3),
                                "stdoutSha256": digest(stdout), "stderrSha256": digest(stderr)})
    if code or timed_out:
        raise RuntimeError(f"{label} failed; retained output: {stdout.name}, {stderr.name}")
    return stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="just-archived Covalent.app; never modified")
    parser.add_argument("output", type=Path, help="new directory for the QA app and evidence")
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("requires Apple Silicon macOS")
    if not args.app.is_absolute() or args.app.is_symlink() or not args.app.is_dir():
        parser.error("app must be an absolute, real directory")
    app = args.app.resolve(strict=True)
    if not args.output.is_absolute() or args.output.exists() or args.output.is_symlink():
        parser.error("output must be a new absolute path")
    parent = args.output.parent.resolve(strict=True)
    output = parent / args.output.name
    if output == app or app in output.parents:
        parser.error("output must be outside the archived app")
    output.mkdir(mode=0o700)
    before = inventory(app)
    receipt = {"schemaVersion": 1, "test": TEST_NAME, "commands": [],
               "archivedAppInventorySha256": hashlib.sha256(
                   json.dumps(before, sort_keys=True).encode()).hexdigest(),
               "productionAppModified": None, "result": "failed",
               "sourceInputs": {p.relative_to(REPO).as_posix(): digest(p) for p in (
                   Path(__file__).resolve(), PARENT_SOURCE, PARENT_ENTITLEMENTS,
                   INHERITED_ENTITLEMENTS, REPO / "crates/covalent-node/tests/one_way_links.rs")}}
    environment = dict(os.environ, COVALENT_REQUIRE_HARDENED_RUNTIME="true")
    try:
        run_logged([str(REPO / "scripts/verify-apple-silicon-bundle.sh"), str(app)],
                   output, "archive-before", receipt, environment=environment)
        messages = run_logged(["cargo", "test", "--locked", "-p", "covalent-node", "--test",
                               "one_way_links", "--no-run", "--message-format=json"],
                              output, "cargo-build", receipt, timeout=900)
        original_test = test_executable(messages.read_text())
        original_test_digest = digest(original_test)
        qa_app = output / "CovalentWorkerGateQA.app"
        contents = qa_app / "Contents"
        executables = contents / "MacOS"
        executables.mkdir(parents=True, mode=0o700)
        (contents / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "life.michaelwong.covalent.worker-gate-qa",
            "CFBundleName": "CovalentWorkerGateQA", "CFBundleExecutable": "sandboxed-worker-parent",
            "CFBundlePackageType": "APPL", "CFBundleVersion": "1"}))
        copies = {}
        for name in ("covalent-rclone", "covalent-engine-guardian"):
            source = app / "Contents/MacOS" / name
            destination = executables / name
            shutil.copy2(source, destination)
            copies[name] = digest(source)
            if digest(destination) != copies[name]:
                raise RuntimeError("packaged executable copy differs")
        receipt["archivedExecutables"] = copies
        inherited_test = executables / "one_way_links"
        shutil.copy2(original_test, inherited_test)
        launcher = executables / "sandboxed-worker-parent"
        run_logged(["xcrun", "clang", "-arch", "arm64", "-Os", "-Wall", "-Wextra", "-Werror",
                    "-fobjc-arc", "-framework", "Foundation", str(PARENT_SOURCE), "-o", str(launcher)],
                   output, "compile-parent", receipt)
        run_logged(["codesign", "--force", "--options", "runtime", "--sign", "-", "--entitlements",
                    str(INHERITED_ENTITLEMENTS), "--timestamp=none", str(inherited_test)],
                   output, "sign-test-copy", receipt)
        run_logged(["codesign", "--force", "--options", "runtime", "--sign", "-", "--entitlements",
                    str(PARENT_ENTITLEMENTS), "--timestamp=none", str(qa_app)],
                   output, "sign-qa-parent", receipt)
        run_logged(["codesign", "--verify", "--deep", "--strict", str(qa_app)],
                   output, "verify-qa-app", receipt)
        for label, binary, expected in (
            ("parent", launcher, PARENT_ENTITLEMENTS),
            ("test", inherited_test, INHERITED_ENTITLEMENTS),
        ):
            actual = run_logged(["codesign", "-d", "--entitlements", ":-", str(binary)],
                                output, f"read-{label}-entitlements", receipt)
            if plistlib.loads(actual.read_bytes()) != plistlib.loads(expected.read_bytes()):
                raise RuntimeError(f"signed {label} entitlement dictionary differs")
        sentinel = output / "outside-sandbox-sentinel"
        sentinel.write_bytes(b"synthetic outside-sandbox read probe\n")
        sentinel_digest = digest(sentinel)
        # The parent passes only HOME, TMPDIR, fixed PATH and bundle-relative
        # test helper paths to children. No account credentials reach the test.
        runtime_environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
        log = run_logged([str(launcher), str(sentinel)], output, "address-handoff", receipt,
                         timeout=300, environment=runtime_environment)
        combined = log.read_text() + (output / "address-handoff.stderr.log").read_text()
        for marker in (
            "SANDBOX_WORKER_GATE outside_read_denied=true private_temp_writable=true",
            "SANDBOX_WORKER_GATE packaged_worker_started=true",
            "ADDRESS_HANDOFF_PHASE initial_run complete=true generation=1",
            "ADDRESS_HANDOFF_PHASE receiver_run complete=true generation=2",
            "ADDRESS_HANDOFF_PHASE sender_run complete=true generation=3",
            "ADDRESS_HANDOFF_RESULT receiver_peer_and_worker_moved=true sender_peer_and_worker_moved=true identities_retained=true generation=3 exact_replays=true manual_stopped=true deletion_policy_retained=true original_preserved=true",
            "test result: ok. 1 passed; 0 failed;",
        ):
            if marker not in combined:
                raise RuntimeError(f"required sandboxed address gate marker missing: {marker}")
        if not re.search(r"^ADDRESS_HANDOFF_CLEANUP temp_root=\S+ runtimes_stopped=true temp_root_absent=true$",
                         combined, re.MULTILINE):
            raise RuntimeError("sandboxed address gate did not confirm worker and fixture cleanup")
        if digest(sentinel) != sentinel_digest:
            raise RuntimeError("outside-sandbox sentinel changed")
        for name, expected in copies.items():
            if digest(executables / name) != expected:
                raise RuntimeError("packaged executable signature or bytes changed")
        if digest(original_test) != original_test_digest:
            raise RuntimeError("original cargo test executable changed")
        run_logged([str(REPO / "scripts/verify-apple-silicon-bundle.sh"), str(app)],
                   output, "archive-after", receipt, environment=environment)
        receipt["result"] = "passed"
        receipt["sandboxRestrictionObserved"] = True
        receipt["privateTemporaryStorageWritable"] = True
        receipt["packagedExecutablesUnchanged"] = True
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        receipt["failure"] = str(error)
    finally:
        receipt["productionAppModified"] = inventory(app) != before
        if receipt["productionAppModified"]:
            receipt["result"] = "failed"
            receipt["failure"] = "production app inventory changed"
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"result": receipt["result"], "evidence": str(output / "receipt.json")}))
    return 0 if receipt["result"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
