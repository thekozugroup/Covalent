#!/usr/bin/env python3
"""Exercise the real shared Gradle tasks with isolated offline Maven artifacts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parent.parent


class InventoryTests(unittest.TestCase):
    def test_task_inputs_license_policy_and_checksum_refresh(self):
        with tempfile.TemporaryDirectory(prefix="interlink-inventory-") as temp:
            root = Path(temp)
            (root / "settings.gradle").write_text("rootProject.name = 'inventory-fixture'\n")
            script = (ROOT / "apps/android/gradle/android-inventory.gradle").as_posix()
            (root / "build.gradle").write_text("""
class FixtureAndroid { def defaultConfig = new Expando(versionName: 'unset') }
extensions.create('android', FixtureAndroid)
android.defaultConfig.versionName = providers.gradleProperty('versionName').getOrElse('1.0.0')
configurations { releaseRuntimeClasspath }
repositories { maven { url = uri('repo') } }
dependencies { releaseRuntimeClasspath "${providers.gradleProperty('group').getOrElse('androidx.fixture')}:runtime:1.0@aar" }
ext.androidInventoryName = 'fixture-android'
ext.androidInventoryDirectory = 'fixture'
tasks.register('assembleRelease')
tasks.register('bundleRelease')
apply from: '%s'
""" % script)
            for group in ("androidx.fixture", "androidx.other", "unreviewed.vendor"):
                artifact = root / "repo" / group.replace(".", "/") / "runtime/1.0/runtime-1.0.aar"
                artifact.parent.mkdir(parents=True)
                with zipfile.ZipFile(artifact, "w") as archive:
                    archive.writestr("jni/arm64-v8a/libfixture.so", b"synthetic native object")
                artifact.with_suffix(".pom").write_text(f"<project><modelVersion>4.0.0</modelVersion><groupId>{group}</groupId><artifactId>runtime</artifactId><version>1.0</version></project>")
            apk = root / "build/outputs/apk/release/fixture.apk"
            apk.parent.mkdir(parents=True)
            apk.write_bytes(b"first candidate")
            report = root / "build/reports/fixture"

            def gradle(*arguments, success=True):
                result = subprocess.run([str(ROOT / "apps/android/gradlew"), "-p", str(root),
                    "--offline", "--console=plain", "--max-workers=2", "-Dorg.gradle.jvmargs=-Xmx512m",
                    *arguments], capture_output=True, text=True, timeout=90, env=os.environ)
                self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
                return result.stdout + result.stderr

            gradle("generateReleaseChecksums")
            original = json.loads((report / "android-sbom.cdx.json").read_text())
            self.assertEqual(original["metadata"]["component"]["version"], "1.0.0")
            self.assertEqual(original["components"][0]["group"], "androidx.fixture")
            native = json.loads((report / "android-license-inventory.json").read_text())["components"][0]["nativeObjects"]
            self.assertEqual(native[0]["sha256"], hashlib.sha256(b"synthetic native object").hexdigest())
            log = gradle("generateReleaseChecksums")
            self.assertIn(":generateAndroidSbom UP-TO-DATE", log)
            self.assertIn(":generateReleaseChecksums UP-TO-DATE", log)
            apk.write_bytes(b"changed candidate")
            gradle("generateReleaseChecksums")
            self.assertIn(hashlib.sha256(apk.read_bytes()).hexdigest() + "  outputs/apk/release/fixture.apk",
                          (report / "SHA256SUMS").read_text())
            gradle("generateAndroidSbom", "-PversionName=2.0.0")
            self.assertEqual(json.loads((report / "android-sbom.cdx.json").read_text())["metadata"]["component"]["version"], "2.0.0")
            gradle("generateAndroidSbom", "-Pgroup=androidx.other")
            self.assertEqual(json.loads((report / "android-sbom.cdx.json").read_text())["components"][0]["group"], "androidx.other")
            log = gradle("generateAndroidSbom", "-Pgroup=unreviewed.vendor", success=False)
            self.assertIn("Unreviewed Android runtime dependency license", log)
            # An AAR can contain duplicate native entries with the same path.
            # License approval does not excuse ambiguous packaged object bytes.
            artifact = root / "repo/androidx/fixture/runtime/1.0/runtime-1.0.aar"
            import warnings
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                with zipfile.ZipFile(artifact, "a") as archive:
                    archive.writestr("jni/arm64-v8a/libfixture.so", b"duplicate")
            log = gradle("generateAndroidSbom", "--refresh-dependencies", success=False)
            self.assertIn("malformed native entries", log)


if __name__ == "__main__":
    unittest.main()
