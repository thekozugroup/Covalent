import org.gradle.api.GradleException

plugins {
    id("com.android.application")
}

val sourcePath = providers.gradleProperty("syncthingSourceDir")
    .orElse(providers.environmentVariable("SYNCTHING_SOURCE_DIR"))
val sourceDir = sourcePath.orNull?.let(::file)
val generatedRoot = layout.buildDirectory.dir("generated/syncthingProof")

android {
    namespace = "life.michaelwong.covalent.engineproof"
    compileSdk = 37

    defaultConfig {
        applicationId = "life.michaelwong.covalent.engineproof"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "0.0.1-proof"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk {
            abiFilters += setOf("arm64-v8a", "x86_64")
        }
    }

    packaging {
        jniLibs {
            // The experiment specifically tests an immutable helper extracted by the
            // package installer. It may not be copied into writable app storage.
            useLegacyPackaging = true
            keepDebugSymbols += "**/libsyncthing.so"
        }
    }

    sourceSets {
        getByName("main").jniLibs.directories.add(
            generatedRoot.get().dir("jniLibs").asFile.absolutePath,
        )
        getByName("main").assets.srcDir(generatedRoot.map { it.dir("assets") })
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

val buildSyncthingHelpers = tasks.register<Exec>("buildSyncthingHelpers") {
    group = "build"
    description = "Cross-builds the exact official Syncthing v2.1.3 helper for the proof APK."
    val checkedSource = sourceDir ?: throw GradleException(
        "Set -PsyncthingSourceDir=/absolute/path or SYNCTHING_SOURCE_DIR to the exact " +
            "official Syncthing v2.1.3 checkout.",
    )
    workingDir = rootProject.projectDir
    commandLine(
        rootProject.projectDir.resolve("build-syncthing-android.sh"),
        checkedSource.canonicalPath,
        generatedRoot.get().asFile.canonicalPath,
    )
    inputs.files(
        rootProject.projectDir.resolve("build-syncthing-android.sh"),
        checkedSource.resolve("build.go"),
        checkedSource.resolve("go.mod"),
        checkedSource.resolve("go.sum"),
    )
    inputs.dir(checkedSource.resolve("cmd"))
    inputs.dir(checkedSource.resolve("lib"))
    outputs.dir(generatedRoot)
    // This is a feasibility gate, not a production incremental build. Always revalidate the
    // checkout/toolchain and regenerate both hashes so stale helpers cannot satisfy a rerun.
    outputs.upToDateWhen { false }
}

tasks.named("preBuild").configure { dependsOn(buildSyncthingHelpers) }

dependencies {
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
}
