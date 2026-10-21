"""Bind final production, QA, retained failures and immutable evidence; no device writes."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import xml.etree.ElementTree as ET

BASE = Path(__file__).resolve().parent
ROOT = BASE.parents[2]
def digest(path):
    with path.open("rb") as stream:
        h = hashlib.sha256()
        while data := stream.read(1024 * 1024):
            h.update(data)
        return h.hexdigest()
def read(relative):
    return json.loads((BASE / relative).read_text())

native = read("native-picker-readiness/receipt.json")
assert native["passed"] is False and native["phase"] == "failed-state-retained"
assert native["instrumentationExitCode"] == 0 and native["resultValidationExitCode"] == 1
assert "Manual Run Now after native address refresh did not complete before its deadline." in (BASE / "native-picker-readiness/instrumentation.log").read_text()
assert native["before"] == native["after"] and native["sourcesUnchanged"]
assert native["previousFailedReceiptUnchanged"] and not native["appReinstalled"] and native["testUpdated"]
assert native["previousFailedReceiptSha256"] == digest(BASE / "native-final-fresh/receipt.json")
assert all(digest(ROOT / name) == value for name, value in native["sourceHashes"].items())
assert read("native-final-fresh/receipt.json")["passed"] is False
assert read("native-strict/receipt.json")["passed"] is False
assert read("native/receipt.json")["passed"] is False
previous = read("native-continuation/receipt.json")
assert previous["passed"] and previous["before"] == previous["after"]
assert previous["previousFailedReceiptSha256"] == digest(BASE / "native/receipt.json")
for name in ("CovalentNodeClient.kt", "FolderAcceptanceRetry.kt"):
    relative = "apps/android/app/src/main/java/life/michaelwong/covalent/data/" + name
    assert digest(BASE / "before-explicit-contract" / (name + ".source")) == previous["sourceHashes"][relative]
old = subprocess.check_output(["git", "show", "69ff050eaac0cb2a896e6b281b6b71a095b1ea33:apps/android/app/src/main/java/life/michaelwong/covalent/data/CovalentNodeClient.kt"], cwd=ROOT)
assert old == (BASE / "baseline/CovalentNodeClient.kt.source").read_bytes()
baseline = ET.parse(BASE / "baseline/test-result.xml").getroot()
assert baseline.get("tests") == "1" and baseline.get("failures") == "1"
assert "BUILD SUCCESSFUL" in (BASE / "strict-gradle.log").read_text()
assert "BUILD FAILED" in (BASE / "picker-readiness-gradle.log").read_text()
assert "BUILD SUCCESSFUL" in (BASE / "picker-readiness-local-gradle.log").read_text()
failed_fresh = read("native-final-fresh/receipt.json")
assert digest(BASE / "native-final-fresh/SafFolderSyncJourneyInstrumentedTest.kt.source") == failed_fresh["sourceHashes"]["apps/android/app/src/androidTest/java/life/michaelwong/covalent/node/SafFolderSyncJourneyInstrumentedTest.kt"]
build = read("android-build-result.json")
assert (build["tests"], build["failures"], build["errors"], build["skipped"]) == (222, 0, 0, 0)
assert len(build["suites"]) == 27
for suite in build["suites"]:
    path = BASE / "reports" / ("TEST-" + suite["suite"] + ".xml")
    assert digest(path) == suite["sha256"]
    report = ET.parse(path).getroot()
    assert int(report.get("tests")) == suite["tests"]
    assert all(int(report.get(key, "0")) == 0 for key in ("failures", "errors", "skipped"))
for artifact in build["artifacts"].values():
    assert digest(ROOT / artifact["path"]) == artifact["sha256"]
assert build["artifacts"]["covalentDebug"]["sha256"] == native["apkSha256"]
assert build["artifacts"]["covalentInstrumentation"]["sha256"] == native["testApkSha256"]
packages = read("package-result.json")
for name, key in (("covalent", "covalentDebug"), ("interlink", "interlinkDebug")):
    assert packages[name]["packageExitCode"] == packages[name]["signatureExitCode"] == 0
    assert packages[name]["sha256"] == build["artifacts"][key]["sha256"]
paths = read("native-directory-result.json")
assert paths["testsPerSurface"] == 7 and paths["sourcesUnchanged"]
assert all(result["exitCode"] == 0 for result in paths["results"].values())
assert all(digest(ROOT / name) == value for name, value in paths["sourceHashes"].items())
prior_path = ROOT / "artifacts/evidence/native-report-parent-header/manifest.json"
prior = json.loads(prior_path.read_text())
assert all(digest(ROOT / name) == value for name, value in prior["files"].items())
for avd in ("avd", "avd-strict"):
    snapshot = BASE / avd / "emulator-after-journey.log"
    assert not snapshot.exists()
    snapshot.write_bytes((BASE / avd / "emulator.log").read_bytes())
source_names = set(native["sourceHashes"]) | {
    "apps/android/app/src/test/java/life/michaelwong/covalent/data/FolderAcceptanceRetryTest.kt",
    "apps/android/app/src/test/java/life/michaelwong/covalent/sync/FolderSyncClientTest.kt",
    "scripts/test-android-native-library-directory.py", "docs/android-folder-acceptance.md", "working.md",
}
pattern = re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{30,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|AIza[A-Za-z0-9_-]{30,}|tskey-(?:auth|api)-[A-Za-z0-9_-]{15,})")
files = {}
for path in sorted(BASE.rglob("*")):
    if not path.is_file() or path.name in ("manifest.json", "emulator.log", "commit.json", "delivery.json"):
        continue
    assert not path.is_symlink()
    if path.suffix != ".png":
        assert not pattern.search(path.read_bytes()), "Credential pattern detected; raw value omitted"
    files[str(path.relative_to(ROOT))] = digest(path)
result = {
    "format": 1, "parentCommit": "69ff050eaac0cb2a896e6b281b6b71a095b1ea33",
    "scope": "Bounded explicit pre-mutation folder acceptance retry and strict native QA readiness/ABI support",
    "sources": {name: digest(ROOT / name) for name in sorted(source_names)}, "files": files,
    "jvmTests": 222, "jvmSuites": 27, "nativePathTestsPerSurface": 7,
    "platforms": ["macOS", "Linux", "Android37 ARM64 emulator"],
    "actualSafJourneyPassed": False, "initialPairingAcceptanceAndManualTransferPassed": True,
    "authoritativeAddressRefreshPassed": True, "addressChangeTransferAccepted": False,
    "addressChangeFailureCause": "unknown; exact-file arrival deadline failed", "nativeContentionObserved": "not established; deterministic HTTP regression proves retry",
    "originalFailuresRetained": True, "priorAcceptedEvidenceFilesUnchanged": len(prior["files"]),
    "priorAcceptedManifestSha256": digest(prior_path), "physicalDeviceAccepted": False,
    "photosCloudCleanupAccepted": False, "imageChanged": False,
    "productionSigningAccepted": False, "releasePromoted": False,
}
manifest = BASE / "manifest.json"
assert not manifest.exists()
manifest.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
print(json.dumps({"manifestSha256": digest(manifest), "sources": len(result["sources"]), "evidenceFiles": len(files)}))
