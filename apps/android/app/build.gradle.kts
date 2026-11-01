import org.gradle.api.GradleException

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
val syncEngineGeneratedRoot = layout.buildDirectory.dir("generated/syncEngine")
val releaseSigningReady = listOf(
    releaseKeystorePath,
    releaseStorePassword,
    releaseKeyAlias,
    releaseKeyPassword,
).all { !it.isNullOrBlank() }


android {
    namespace = "life.michaelwong.covalent"
    compileSdk = 37

    defaultConfig {
        applicationId = "life.michaelwong.covalent"
        minSdk = 26
        targetSdk = 37
        versionCode = 2012
        versionName = "0.2.12"
        buildConfigField("int", "COVALENT_PEER_PORT", "8787")
        buildConfigField("int", "COVALENT_SYNC_PORT", "8789")
        buildConfigField(
            "boolean",
            "COVALENT_SYNC_ENGINE_PACKAGED",
            "false",
        )

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
            buildConfigField("boolean", "COVALENT_SYNC_ENGINE_PACKAGED", "true")
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
        jniLibs {
            // The maintained engine and its guardian execute directly from the
            // installer-owned nativeLibraryDir. They are never copied to writable storage.
            useLegacyPackaging = true
            // The JNI linker already uses --strip-all. Keep AGP from changing the
            // final-link bytes recorded by the native provenance collector.
            keepDebugSymbols += "**/libcovalent_android_jni.so"
            keepDebugSymbols += "**/libcovalentrclone.so"
            keepDebugSymbols += "**/libengineguardian.so"
        }
    }

    sourceSets {
        getByName("main").jniLibs.directories.add(
            layout.buildDirectory.dir("generated/jniLibs").get().asFile.absolutePath,
        )
        getByName("main").jniLibs.directories.add(
            syncEngineGeneratedRoot.get().dir("jniLibs").asFile.absolutePath,
        )
        getByName("main").assets.directories.add(
            syncEngineGeneratedRoot.get().dir("assets").asFile.absolutePath,
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
val rcloneSourceDir = covalentRepoRoot.resolve("packaging/rclone")
// Shared Rust/Android control contract fixtures are test resources only.
android.sourceSets.getByName("test").resources.srcDir(covalentRepoRoot.resolve("crates/covalent-node/tests/fixtures"))

android.buildTypes.getByName("debug").buildConfigField(
    "boolean",
    "COVALENT_SYNC_ENGINE_PACKAGED",
    (providers.gradleProperty("covalentBuildSyncEngine").orNull == "true").toString(),
)

val buildAndroidJni = tasks.register<Exec>("buildAndroidJni") {
    group = "build"
    description = "Builds pinned Android arm64 and x86_64 JNI libraries with 16 KiB ELF alignment."
    workingDir = covalentRepoRoot
    doFirst {
        val generatedRoot = layout.buildDirectory.dir("generated/jniLibs").get().asFile
        project.delete(generatedRoot)
        check(generatedRoot.parentFile.mkdirs() || generatedRoot.parentFile.isDirectory)
    }
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
        covalentRepoRoot.resolve("scripts/collect-android-native-link-provenance.py"),
        covalentRepoRoot.resolve("crates/covalent-android-jni"),
        covalentRepoRoot.resolve("crates/covalent-core"),
        covalentRepoRoot.resolve("crates/covalent-node"),
        covalentRepoRoot.resolve("crates/covalent-protocol"),
    )
    outputs.dir(layout.buildDirectory.dir("generated/jniLibs"))
    // Recheck the external pinned NDK, its exact notices, and final link evidence.
    outputs.upToDateWhen { false }
}

val buildAndroidSyncEngine = tasks.register<Exec>("buildAndroidSyncEngine") {
    group = "build"
    description = "Builds the restricted rclone v1.75.1 worker and reviewed guardian for Android."
    workingDir = covalentRepoRoot
    doFirst {
        check(rcloneSourceDir.isDirectory) {
            "The committed packaging/rclone module is missing."
        }
        val generatedRoot = syncEngineGeneratedRoot.get().asFile
        project.delete(generatedRoot)
        check(generatedRoot.parentFile.mkdirs() || generatedRoot.parentFile.isDirectory)
    }
    commandLine(
        covalentRepoRoot.resolve("scripts/build-android-sync-engine.sh"),
        rcloneSourceDir.canonicalPath,
        syncEngineGeneratedRoot.get().asFile.canonicalPath,
    )
    inputs.files(
        rcloneSourceDir,
        covalentRepoRoot.resolve("scripts/build-android-sync-engine.sh"),
        covalentRepoRoot.resolve("scripts/android-native-budgets.sh"),
        covalentRepoRoot.resolve("scripts/collect-go-target-license-inventory.py"),
        covalentRepoRoot.resolve("scripts/collect-sync-engine-notices.py"),
        covalentRepoRoot.resolve("scripts/collect-android-native-link-provenance.py"),
        covalentRepoRoot.resolve("scripts/android-go-link-wrapper.sh"),
        covalentRepoRoot.resolve("packaging/sync-engine/engine-guardian.c"),
        covalentRepoRoot.resolve("LICENSE"),
    )
    outputs.dir(syncEngineGeneratedRoot)
    // Every requested packaging run must recheck source, toolchain, ELF policy and hashes.
    outputs.upToDateWhen { false }
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
    mustRunAfter(buildAndroidJni, buildAndroidSyncEngine)
}
// Assets use the same generated-root pattern. Keep every variant's merge after
// the producer whenever the opt-in preBuild edge or a release task puts both
// in the graph; this also makes Gradle's generated-directory relationship
// explicit for AGP's validation.
tasks.matching { it.name.startsWith("merge") && it.name.endsWith("Assets") }.configureEach {
    mustRunAfter(buildAndroidSyncEngine)
}

// Lint model writers also consume the generated assets/JNI source roots.
// Ordering is conditional on the producers already being in the graph, so
// ordinary source-only debug lint does not require native build toolchains.
tasks.matching { it.name.contains("lint", ignoreCase = true) }.configureEach {
    mustRunAfter(buildAndroidJni, buildAndroidSyncEngine)
}

tasks.matching { it.name == "assembleRelease" || it.name == "bundleRelease" }.configureEach {
    dependsOn(buildAndroidJni, buildAndroidSyncEngine)
}
tasks.matching { it.name == "preBuild" }.configureEach {
    if (providers.gradleProperty("covalentBuildNative").orNull == "true") dependsOn(buildAndroidJni)
    if (providers.gradleProperty("covalentBuildSyncEngine").orNull == "true") {
        dependsOn(buildAndroidSyncEngine)
    }
}

// Both installed apps use the same fail-closed runtime license/hash policy.
extra["androidInventoryName"] = "covalent-android"
extra["androidInventoryDirectory"] = "covalent"
apply(from = rootProject.file("gradle/android-inventory.gradle"))
val generateReleaseChecksums = tasks.named("generateReleaseChecksums")

generateReleaseChecksums.configure {
    mustRunAfter(verifyReleaseSigningConfigured)
}

tasks.register("prepareReleaseCandidate") {
    group = "distribution"
    description = "Builds signed release artifacts, SBOM, and checksums from secret inputs."
    dependsOn(verifyReleaseSigningConfigured, generateReleaseChecksums)
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.06.00")
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
