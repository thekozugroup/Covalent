plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Separate installed identity; reuse the reviewed engine, grants and pairing code.
android {
    namespace = "life.michaelwong.covalent"
    compileSdk = 37
    defaultConfig {
        applicationId = "life.michaelwong.interlink"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0-dev"
        buildConfigField("int", "COVALENT_PEER_PORT", "18887")
        buildConfigField("int", "COVALENT_SYNC_PORT", "18889")
        buildConfigField("boolean", "COVALENT_SYNC_ENGINE_PACKAGED", "true")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk { abiFilters += setOf("arm64-v8a", "x86_64") }
    }
    buildFeatures { compose = true; buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "../app/proguard-rules.pro")
            // Publishing must configure a real release identity. Never substitute debug signing.
        }
    }
    sourceSets.getByName("main") {
        kotlin.srcDir("../app/src/main/java")
        res.srcDir("../app/src/main/res")
        assets.srcDir("../app/src/main/assets")
        assets.srcDir(project(":app").layout.buildDirectory.dir("generated/syncEngine/assets").get().asFile)
        jniLibs.srcDir(project(":app").layout.buildDirectory.dir("generated/jniLibs").get().asFile)
        jniLibs.srcDir(project(":app").layout.buildDirectory.dir("generated/syncEngine/jniLibs").get().asFile)
    }
    packaging {
        resources.merges += setOf("/META-INF/AL2.0", "/META-INF/LGPL2.1")
        jniLibs {
            useLegacyPackaging = true
            keepDebugSymbols += setOf("**/libcovalent_android_jni.so", "**/libcovalentrclone.so", "**/libengineguardian.so")
        }
    }
}

tasks.named("preBuild") {
    dependsOn(":app:buildAndroidJni", ":app:buildAndroidSyncEngine")
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.06.00"))
    implementation("androidx.compose.material3:material3:1.5.0-alpha29")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.core:core-ktx:1.19.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.work:work-runtime-ktx:2.11.1")
    implementation("androidx.documentfile:documentfile:1.1.0")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    // Keep main and instrumentation classpaths consistent with test-ext 1.3.
    constraints {
        implementation("androidx.concurrent:concurrent-futures:1.2.0")
        implementation("androidx.concurrent:concurrent-futures-ktx:1.2.0")
    }
    androidTestImplementation(platform("androidx.compose:compose-bom:2026.06.00"))
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}

// A separate inventory records this app's Expressive dependency graph.
extra["androidInventoryName"] = "interlink-android"
extra["androidInventoryDirectory"] = "interlink"
apply(from = rootProject.file("gradle/android-inventory.gradle"))

val verifyInterlinkRelease = tasks.register<Exec>("verifyInterlinkRelease") {
    group = "verification"
    description = "Verifies the dedicated app identity, alignment, native bytes and its own dependency evidence."
    dependsOn("assembleDebug", "assembleRelease", "bundleRelease", "generateAndroidSbom")
    workingDir(rootProject.projectDir.resolve("../.."))
    commandLine("sh", "scripts/check-interlink-package.sh")
}
tasks.named("generateReleaseChecksums") { dependsOn(verifyInterlinkRelease) }
