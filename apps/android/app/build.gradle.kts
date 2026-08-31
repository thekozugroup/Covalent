import groovy.json.JsonOutput
import org.gradle.api.GradleException
import java.security.MessageDigest

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

fun releaseSecret(name: String): String? =
    providers.gradleProperty(name).orElse(providers.environmentVariable(name)).orNull

val releaseKeystorePath = releaseSecret("COVALENT_ANDROID_KEYSTORE_PATH")
val releaseStorePassword = releaseSecret("COVALENT_ANDROID_STORE_PASSWORD")
val releaseKeyAlias = releaseSecret("COVALENT_ANDROID_KEY_ALIAS")
val releaseKeyPassword = releaseSecret("COVALENT_ANDROID_KEY_PASSWORD")
val releaseSigningReady = listOf(
    releaseKeystorePath,
    releaseStorePassword,
    releaseKeyAlias,
    releaseKeyPassword,
).all { !it.isNullOrBlank() }

data class ReviewedDependencyLicense(
    val spdxId: String,
    val name: String,
    val textUrl: String,
    val evidenceUrl: String,
)

fun reviewedDependencyLicense(group: String, name: String, version: String): ReviewedDependencyLicense {
    val path = "${group.replace('.', '/')}/$name/$version/$name-$version.pom"
    val pomUrl = when {
        group.startsWith("androidx.") -> "https://dl.google.com/dl/android/maven2/$path"
        else -> "https://repo1.maven.org/maven2/$path"
    }
    val evidenceUrl = when {
        group == "com.google.guava" && name == "listenablefuture" ->
            "https://repo1.maven.org/maven2/com/google/guava/guava-parent/26.0-android/guava-parent-26.0-android.pom"
        else -> pomUrl
    }
    val reviewedAsApache2 =
        group.startsWith("androidx.") ||
            group == "com.google.guava" && name == "listenablefuture" ||
            group == "org.jetbrains" && name == "annotations" ||
            group.startsWith("org.jetbrains.kotlin") ||
            group == "org.jspecify" && name == "jspecify"
    if (!reviewedAsApache2) {
        throw GradleException(
            "Unreviewed Android runtime dependency license: $group:$name:$version. " +
                "Add an explicit reviewed mapping before producing a release candidate.",
        )
    }
    return ReviewedDependencyLicense(
        spdxId = "Apache-2.0",
        name = "Apache License 2.0",
        textUrl = "https://www.apache.org/licenses/LICENSE-2.0.txt",
        evidenceUrl = evidenceUrl,
    )
}

android {
    namespace = "life.michaelwong.covalent"
    compileSdk = 37

    defaultConfig {
        applicationId = "life.michaelwong.covalent"
        minSdk = 26
        targetSdk = 37
        versionCode = 2000
        versionName = "0.2.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        vectorDrawables.useSupportLibrary = true
        ndk {
            abiFilters += setOf("arm64-v8a", "x86_64")
        }
    }

    signingConfigs {
        if (releaseSigningReady) {
            create("release") {
                storeFile = file(checkNotNull(releaseKeystorePath))
                storePassword = checkNotNull(releaseStorePassword)
                keyAlias = checkNotNull(releaseKeyAlias)
                keyPassword = checkNotNull(releaseKeyPassword)
                enableV1Signing = true
                enableV2Signing = true
                enableV3Signing = true
                enableV4Signing = true
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            if (releaseSigningReady) signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        // Preserve dependency license text in distributable artifacts.
        resources.merges += setOf("/META-INF/AL2.0", "/META-INF/LGPL2.1")
    }

    sourceSets {
        getByName("main").jniLibs.directories.add(
            layout.buildDirectory.dir("generated/jniLibs").get().asFile.absolutePath,
        )
    }

    lint {
        abortOnError = true
        warningsAsErrors = true
        checkDependencies = true
        lintConfig = file("lint.xml")
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
    }
}

val verifyReleaseSigningConfigured = tasks.register("verifyReleaseSigningConfigured") {
    group = "verification"
    description = "Fails unless all secret-backed Android release-signing inputs are configured."
    doLast {
        check(releaseSigningReady) {
            "Set COVALENT_ANDROID_KEYSTORE_PATH, COVALENT_ANDROID_STORE_PASSWORD, " +
                "COVALENT_ANDROID_KEY_ALIAS, and COVALENT_ANDROID_KEY_PASSWORD."
        }
        check(file(checkNotNull(releaseKeystorePath)).isFile) {
            "COVALENT_ANDROID_KEYSTORE_PATH does not identify a readable keystore file."
        }
    }
}

// The Gradle root project lives at `apps/android`, so `projectDir.parentFile` is
// `apps` — not the repository root. Resolving the Cargo workspace against it
// produced `apps/crates/covalent-android-jni` and `apps/Cargo.lock`, so the JNI
// Exec task ran from the wrong directory and `assembleRelease` always failed.
// Walk up to the directory that actually owns the workspace instead of hardcoding
// a parent count, and fail closed with a clear message if it cannot be found.
val covalentRepoRoot: File =
    generateSequence(rootProject.projectDir.canonicalFile) { it.parentFile }
        .firstOrNull {
            it.resolve("Cargo.lock").isFile &&
                it.resolve("crates/covalent-android-jni").isDirectory &&
                it.resolve("scripts/build-android-jni.sh").isFile
        }
        ?: error(
            "Unable to locate the Covalent repository root above ${rootProject.projectDir}: " +
                "expected an ancestor containing Cargo.lock, crates/covalent-android-jni, " +
                "and scripts/build-android-jni.sh."
        )

val buildAndroidJni = tasks.register<Exec>("buildAndroidJni") {
    group = "build"
    description = "Builds pinned Android arm64 and x86_64 JNI libraries with 16 KiB ELF alignment."
    workingDir = covalentRepoRoot
    commandLine("./scripts/build-android-jni.sh", layout.buildDirectory.dir("generated/jniLibs").get().asFile.absolutePath)
    // The JNI archive links the node, core, and protocol crates too. Tracking
    // only the bridge crate made Gradle eligible to reuse native output after a
    // transitive Rust or linker-script edit, despite the release gate promising
    // source-current binaries. Declare every source and link-policy input.
    inputs.files(
        covalentRepoRoot.resolve("Cargo.toml"),
        covalentRepoRoot.resolve("Cargo.lock"),
        covalentRepoRoot.resolve("rust-toolchain.toml"),
        covalentRepoRoot.resolve("scripts/build-android-jni.sh"),
        covalentRepoRoot.resolve("crates/covalent-android-jni"),
        covalentRepoRoot.resolve("crates/covalent-core"),
        covalentRepoRoot.resolve("crates/covalent-node"),
        covalentRepoRoot.resolve("crates/covalent-protocol"),
    )
    outputs.dir(layout.buildDirectory.dir("generated/jniLibs"))
}

// `sourceSets.main.jniLibs.directories` above takes a plain path string, so
// Gradle cannot infer that `buildAndroidJni` produces that directory and fails
// validation with "uses this output ... without declaring an explicit or
// implicit dependency". Declare the relationship. `mustRunAfter` rather than
// `dependsOn` is deliberate: it establishes correct ordering whenever
// `buildAndroidJni` is in the task graph without forcing the Rust/NDK build into
// every debug build, preserving the existing opt-in `covalentBuildNative`
// behaviour and the unconditional `assembleRelease` dependency below.
tasks.matching { it.name.endsWith("JniLibFolders") }.configureEach {
    mustRunAfter(buildAndroidJni)
}

tasks.matching { it.name == "assembleRelease" }.configureEach { dependsOn(buildAndroidJni) }
tasks.matching { it.name == "preBuild" }.configureEach {
    if (providers.gradleProperty("covalentBuildNative").orNull == "true") dependsOn(buildAndroidJni)
}

val generateAndroidSbom = tasks.register("generateAndroidSbom") {
    group = "reporting"
    description = "Generates deterministic, fail-closed Android dependency, license, and hash inventories."
    val output = layout.buildDirectory.file("reports/covalent/android-sbom.cdx.json")
    val licenseOutput = layout.buildDirectory.file("reports/covalent/android-license-inventory.json")
    inputs.files(configurations.getByName("releaseRuntimeClasspath"))
    outputs.files(output, licenseOutput)
    doLast {
        val artifacts = configurations.getByName("releaseRuntimeClasspath")
            .resolvedConfiguration
            .resolvedArtifacts
            .distinctBy { it.file.canonicalPath }
            .sortedWith(compareBy({ it.moduleVersion.id.toString() }, { it.file.name }))
        fun sha256(file: File): String {
            val digest = MessageDigest.getInstance("SHA-256")
            file.inputStream().buffered().use { input ->
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    digest.update(buffer, 0, count)
                }
            }
            return digest.digest().joinToString("") { "%02x".format(it) }
        }
        val components = artifacts.map { artifact ->
            val group = artifact.moduleVersion.id.group
            val name = artifact.name
            val version = artifact.moduleVersion.id.version
            val license = reviewedDependencyLicense(group, name, version)
            val artifactHash = sha256(artifact.file)
            val artifactType = artifact.file.extension.ifBlank { "jar" }
            linkedMapOf<String, Any>(
                "type" to "library",
                "group" to group,
                "name" to name,
                "version" to version,
                "bom-ref" to "pkg:maven/$group/$name@$version?type=$artifactType",
                "hashes" to listOf(mapOf("alg" to "SHA-256", "content" to artifactHash)),
                "licenses" to listOf(
                    mapOf(
                        "license" to linkedMapOf(
                            "id" to license.spdxId,
                            "url" to license.textUrl,
                        ),
                    ),
                ),
                "externalReferences" to listOf(
                    mapOf("type" to "other", "url" to license.evidenceUrl),
                ),
                "properties" to listOf(
                    mapOf("name" to "covalent:artifact-file", "value" to artifact.file.name),
                    mapOf("name" to "covalent:license-review", "value" to "explicit-fail-closed"),
                ),
            )
        }
        val document = linkedMapOf<String, Any>(
            "bomFormat" to "CycloneDX",
            "specVersion" to "1.5",
            "version" to 1,
            "metadata" to linkedMapOf(
                "component" to linkedMapOf(
                    "type" to "application",
                    "group" to "life.michaelwong",
                    "name" to "covalent-android",
                    "version" to android.defaultConfig.versionName.orEmpty(),
                ),
                "properties" to listOf(
                    mapOf("name" to "covalent:license-metadata-preserved", "value" to "true"),
                    mapOf("name" to "covalent:license-policy", "value" to "fail-closed-explicit-review"),
                ),
            ),
            "components" to components,
        )
        output.get().asFile.apply {
            parentFile.mkdirs()
            writeText(JsonOutput.prettyPrint(JsonOutput.toJson(document)) + "\n")
        }
        val licenseInventory = linkedMapOf<String, Any>(
            "schemaVersion" to 1,
            "policy" to "Every release runtime artifact requires an explicit reviewed SPDX license mapping.",
            "preservedPackageMetadata" to listOf("META-INF/AL2.0", "META-INF/LGPL2.1"),
            "components" to artifacts.map { artifact ->
                val group = artifact.moduleVersion.id.group
                val name = artifact.name
                val version = artifact.moduleVersion.id.version
                val license = reviewedDependencyLicense(group, name, version)
                linkedMapOf(
                    "coordinate" to "$group:$name:$version",
                    "artifact" to artifact.file.name,
                    "sha256" to sha256(artifact.file),
                    "spdxId" to license.spdxId,
                    "licenseName" to license.name,
                    "licenseText" to license.textUrl,
                    "reviewEvidence" to license.evidenceUrl,
                )
            },
        )
        licenseOutput.get().asFile.apply {
            parentFile.mkdirs()
            writeText(JsonOutput.prettyPrint(JsonOutput.toJson(licenseInventory)) + "\n")
        }
    }
}

val generateReleaseChecksums = tasks.register("generateReleaseChecksums") {
    group = "distribution"
    description = "Writes SHA-256 checksums for exact Android release-candidate artifacts."
    dependsOn("assembleRelease", "bundleRelease", generateAndroidSbom)
    val output = layout.buildDirectory.file("reports/covalent/SHA256SUMS")
    outputs.file(output)
    doLast {
        val roots = listOf(
            layout.buildDirectory.dir("outputs/apk/release").get().asFile,
            layout.buildDirectory.dir("outputs/bundle/release").get().asFile,
            layout.buildDirectory.dir("outputs/mapping/release").get().asFile,
            layout.buildDirectory.dir("reports/covalent").get().asFile,
        )
        val files = roots.asSequence()
            .filter(File::exists)
            .flatMap { root -> root.walkTopDown().filter(File::isFile) }
            .filter { it != output.get().asFile }
            .distinctBy { it.canonicalPath }
            .sortedBy { it.relativeTo(layout.buildDirectory.get().asFile).invariantSeparatorsPath }
            .toList()
        val digest = MessageDigest.getInstance("SHA-256")
        val lines = files.map { file ->
            digest.reset()
            file.inputStream().buffered().use { input ->
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    digest.update(buffer, 0, count)
                }
            }
            val hash = digest.digest().joinToString("") { "%02x".format(it) }
            val path = file.relativeTo(layout.buildDirectory.get().asFile).invariantSeparatorsPath
            "$hash  $path"
        }
        output.get().asFile.apply {
            parentFile.mkdirs()
            writeText(lines.joinToString("\n", postfix = "\n"))
        }
    }
}

generateReleaseChecksums.configure {
    mustRunAfter(verifyReleaseSigningConfigured)
}

tasks.register("prepareReleaseCandidate") {
    group = "distribution"
    description = "Builds signed release artifacts, SBOM, and checksums from secret inputs."
    dependsOn(verifyReleaseSigningConfigured, generateReleaseChecksums)
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.08.00")
    implementation(composeBom)
    androidTestImplementation(composeBom)

    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.core:core-ktx:1.19.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.work:work-runtime-ktx:2.11.1")
    implementation("androidx.documentfile:documentfile:1.1.0")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20250517")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
    testImplementation("com.squareup.okhttp3:okhttp-tls:4.12.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.7.0")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}
