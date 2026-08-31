package life.michaelwong.covalent

import java.io.File
import life.michaelwong.covalent.node.KeyProtectionLevel
import life.michaelwong.covalent.ui.keyProtectionCopyRes
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The Kotlin/Rust key-protection contract.
 *
 * These assertions are deliberately written against **literals and the Rust source**, not
 * against the Kotlin enum's own values. A test that read `KeyProtectionLevel.SOFTWARE
 * .wireValue` and compared it to itself would pass no matter what the number became, and
 * a wire value that drifts is exactly the bug that would make the Rust side read
 * "unprotected" as "protected" or the reverse.
 */
class KeyProtectionContractTest {

    @Test
    fun wireValuesArePinnedToTheirContractNumbers() {
        assertEquals(0, KeyProtectionLevel.UNAVAILABLE.wireValue)
        assertEquals(1, KeyProtectionLevel.SOFTWARE.wireValue)
        assertEquals(2, KeyProtectionLevel.TRUSTED_ENVIRONMENT.wireValue)
        assertEquals(3, KeyProtectionLevel.STRONGBOX.wireValue)
        assertEquals(
            "A new protection level must be appended, never renumbered",
            4,
            KeyProtectionLevel.entries.size,
        )
    }

    @Test
    fun rustDecodesTheSameWireValuesKotlinSends() {
        val rust = rustJniSource()
        mapOf(
            "PROTECTION_UNAVAILABLE" to KeyProtectionLevel.UNAVAILABLE,
            "PROTECTION_SOFTWARE" to KeyProtectionLevel.SOFTWARE,
            "PROTECTION_TRUSTED_ENVIRONMENT" to KeyProtectionLevel.TRUSTED_ENVIRONMENT,
            "PROTECTION_STRONGBOX" to KeyProtectionLevel.STRONGBOX,
        ).forEach { (constant, level) ->
            val declared = Regex("const $constant: i32 = (-?\\d+);").find(rust)
                ?: error("$constant is not declared in the Android JNI crate")
            assertEquals(
                "$constant must match ${level.name}.wireValue across the JNI boundary",
                level.wireValue,
                declared.groupValues[1].toInt(),
            )
        }
    }

    @Test
    fun rustNoLongerHardcodesWhetherProtectionIsAvailable() {
        val rust = rustJniSource()
        assertFalse(
            "The embedded node must decide from the level Kotlin measured, not a constant",
            rust.contains("SECURE_IDENTITY_PROTECTOR_AVAILABLE"),
        )
        assertTrue(
            "start_node must still refuse to run without a protected identity",
            rust.contains("secure_key_protector_required"),
        )
        assertTrue(
            "The admission decision must read the level passed across the boundary",
            rust.contains("identity_protection_accepted(key_protection_level)"),
        )
    }

    /**
     * A mismatched JNI descriptor is a runtime crash, not a compile error, so the arity
     * of the Rust method descriptor is checked against the Kotlin declaration here.
     */
    @Test
    fun theNativeStartDescriptorMatchesTheKotlinDeclaration() {
        val rust = rustJniSource()
        assertTrue(
            "nativeStart's JNI descriptor must carry token, KEK, exact version, quotas, and protection",
            rust.contains("\"(Ljava/lang/String;Ljava/lang/String;Z[B[BIJJI)Ljava/lang/String;\""),
        )
        val kotlin = moduleFile("src/main/java/life/michaelwong/covalent/node/CovalentNative.kt")
            .readText()
        val declaration = Regex("private external fun nativeStart\\(([^)]*)\\)", RegexOption.DOT_MATCHES_ALL)
            .find(kotlin)
            ?: error("nativeStart is not declared in CovalentNative.kt")
        val parameters = declaration.groupValues[1]
            .split(",")
            .map(String::trim)
            .filter(String::isNotEmpty)
        assertEquals(
            "The descriptor (String, String, boolean, token[], KEK[], version, long, long, " +
                "protection) has nine " +
                "parameters; Kotlin declares ${parameters.size}: $parameters",
            9,
            parameters.size,
        )
        assertTrue(
            "The last parameter must be the Int protection level: ${parameters.last()}",
            parameters.last().endsWith(": Int"),
        )
        assertTrue(
            "Native methods must stay registered from JNI_OnLoad, never exported by name",
            rust.contains("register_native_methods") && !rust.contains("Java_life_michaelwong"),
        )
    }

    @Test
    fun everyProtectionLevelGetsItsOwnHonestSentence() {
        assertNull(
            "A device that cannot protect its identity must not be given reassuring copy",
            keyProtectionCopyRes(KeyProtectionLevel.UNAVAILABLE),
        )
        val described = KeyProtectionLevel.entries
            .filter { it != KeyProtectionLevel.UNAVAILABLE }
            .associateWith { checkNotNull(keyProtectionCopyRes(it)) }
        assertEquals(
            "Two protection levels must never share one sentence",
            described.size,
            described.values.distinct().size,
        )
    }

    @Test
    fun theSoftwareFallbackSaysItIsSoftwareAndPromisesNoHardware() {
        val strings = moduleFile("src/main/res/values/strings.xml").readText()
        fun copy(name: String): String =
            Regex("<string name=\"$name\">(.*?)</string>", RegexOption.DOT_MATCHES_ALL)
                .find(strings)?.groupValues?.get(1)
                ?: error("strings.xml is missing $name")

        val software = copy("phone_provider_protection_software")
        assertTrue(
            "The software fallback must say it is software: $software",
            software.contains("software", ignoreCase = true),
        )
        assertFalse(
            "The software fallback must not claim the key never leaves anything: $software",
            software.contains("never leaves", ignoreCase = true),
        )
        listOf("phone_provider_protection_hardware", "phone_provider_protection_strongbox")
            .forEach { name ->
                assertTrue(
                    "$name must state that the key stays in hardware: ${copy(name)}",
                    copy(name).contains("never leaves", ignoreCase = true),
                )
            }
        assertTrue(
            "The blocking message must tell the person what to do",
            copy("phone_provider_locked").contains("Set a screen lock"),
        )
    }

    /**
     * Guards the one CodeQL alert this repository dismisses rather than fixes.
     *
     * `KeyInfo.isInsideSecureHardware` is deprecated, and CodeQL's `java/deprecated-call`
     * is right to say so. It is kept anyway because API 26-30 ships no
     * `KeyInfo.getSecurityLevel()`, so deleting it would report SOFTWARE for every
     * pre-31 device and understate hardware protection that is really there. That
     * argument holds only while `minSdk` is below 31. This test fails the moment it
     * stops holding, so the branch and its dismissal cannot quietly outlive their reason.
     */
    @Test
    fun theDeprecatedSecureHardwareProbeIsStillTheOnlyOptionBelowApi31() {
        val minSdk = Regex("minSdk\\s*=\\s*(\\d+)")
            .find(repositoryFile("apps/android/app/build.gradle.kts").readText())
            ?.groupValues?.get(1)?.toInt()
            ?: error("minSdk is not declared in app/build.gradle.kts")
        assertTrue(
            "minSdk is $minSdk. KeyInfo.getSecurityLevel() exists from API 31, so the " +
                "deprecated isInsideSecureHardware branch in IdentityKeyProtector is now " +
                "dead. Delete it and un-dismiss the java/deprecated-call CodeQL alert.",
            minSdk < 31,
        )

        val protector = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/IdentityKeyProtector.kt",
        ).readText()
        assertTrue(
            "The deprecated probe must stay behind an SDK_INT check, never run on API 31+",
            protector.contains("Build.VERSION.SDK_INT >= Build.VERSION_CODES.S"),
        )
        assertTrue(
            "API 31+ must read the precise security level, not the deprecated boolean",
            protector.contains("info.securityLevel"),
        )
        assertTrue(
            "Only the guarded pre-31 branch may consult isInsideSecureHardware",
            protector.split("isInsideSecureHardware").size - 1 == 1,
        )
    }

    /**
     * The local API bearer token must come from one long-lived, kernel-seeded CSPRNG.
     *
     * Written against the source because the failure mode is a silent downgrade: swapping
     * in `java.util.Random`, or dropping the zeroing of the buffer, changes no signature
     * and breaks no other test, but weakens the credential that guards the node.
     */
    @Test
    fun theLocalApiTokenIsDrawnFromOneReusedCsprng() {
        val manager = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/EmbeddedNodeManager.kt",
        ).readText()
        assertTrue(
            "The token generator must be SecureRandom, never a predictable PRNG",
            manager.contains("import java.security.SecureRandom"),
        )
        assertFalse(
            "java.util.Random and kotlin.random are not acceptable sources for a credential",
            manager.contains("java.util.Random") || manager.contains("kotlin.random"),
        )
        assertNull(
            "Construct SecureRandom once and reuse it; an inline SecureRandom().nextBytes " +
                "is the throwaway-PRNG shape that CodeQL java/random-used-once flags",
            Regex("SecureRandom\\(\\)\\s*\\.").find(manager),
        )
        assertTrue(
            "The reused CSPRNG must be a field of EmbeddedNodeManager",
            manager.contains("private val csprng = SecureRandom()"),
        )
        assertTrue(
            "The token must be 32 random bytes drawn from that field",
            manager.contains("const val TOKEN_RANDOM_BYTES = 32") &&
                manager.contains("csprng.nextBytes(raw)"),
        )
        assertTrue(
            "The raw token buffer must be zeroed once it has been encoded",
            manager.contains("raw.fill(0)") && manager.contains("encoded?.fill(0)"),
        )
    }

    @Test
    fun kekIsCiphertextOnlyFailLockedAndZeroizedAcrossTheBoundary() {
        val protector = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/IdentityKeyProtector.kt",
        ).readText()
        val manager = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/EmbeddedNodeManager.kt",
        ).readText()
        val native = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/CovalentNative.kt",
        ).readText()
        val rust = rustJniSource()

        assertTrue(protector.contains("AtomicFile(") && protector.contains("noBackupFilesDir"))
        assertTrue(protector.contains("kek-envelope.v1"))
        assertTrue(protector.contains("ByteArray(KEK_BYTES)"))
        assertTrue(protector.contains("const val KEK_BYTES = 32"))
        assertTrue(
            "Existing KEK ciphertext must be opened, never overwritten after auth/invalidation failure",
            protector.contains("if (kekEnvelope.baseFile.exists()) return@synchronized openPersistedKek()"),
        )
        assertFalse(
            "Invalidation must never delete the alias and silently rotate durable state",
            protector.contains("deleteEntry(") || protector.contains("fun forget("),
        )
        assertTrue(manager.contains("keyEncryptionKey.close()"))
        assertTrue(native.contains("keyEncryptionKey: ByteArray") && native.contains("keyVersion: Int"))
        assertTrue(rust.contains("take_java_secret(environment, &key_encryption_key)"))
        assertTrue(rust.contains("StaticKeyProtector::new(key_version as u32, kek)"))
        assertTrue(rust.contains("configuration.key_protector = Some(Arc::new(protector))"))
    }

    @Test
    fun api37ProviderEnableAlwaysRequestsLanAndUsesTheConnectedDeviceServiceContract() {
        val manifest = moduleFile("src/main/AndroidManifest.xml").readText()
        val build = moduleFile("build.gradle.kts").readText()
        val ui = moduleFile("src/main/java/life/michaelwong/covalent/ui/CovalentApp.kt").readText()
        val service = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/NodeProviderService.kt",
        ).readText()

        assertTrue(build.contains("compileSdk = 37") && build.contains("targetSdk = 37"))
        assertTrue(manifest.contains("android.permission.ACCESS_LOCAL_NETWORK"))
        assertTrue(manifest.contains("android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE"))
        assertTrue(manifest.contains("android:foregroundServiceType=\"connectedDevice\""))
        assertTrue(manifest.contains("android:name=\".node.NodeProviderService\""))
        assertTrue(manifest.contains("android:exported=\"false\""))
        val providerPermissionGate = ui.substringAfter("fun requestProviderEnable()")
            .substringBefore("val localNetworkPermission")
        assertTrue(providerPermissionGate.contains("Build.VERSION.SDK_INT >= 37"))
        assertTrue(providerPermissionGate.contains("add(LOCAL_NETWORK_PERMISSION)"))
        assertFalse(
            "Peer traffic needs ACCESS_LOCAL_NETWORK even when multicast discovery is off",
            providerPermissionGate.contains("state.providerLanDiscovery && Build.VERSION.SDK_INT >= 37"),
        )
        assertTrue(service.contains("FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE"))
    }

    @Test
    fun legacyMigrationIsAtomicAndNeverTouchesExternalCredentials() {
        val manager = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/EmbeddedNodeManager.kt",
        ).readText()
        val directBoot = moduleFile(
            "src/main/java/life/michaelwong/covalent/node/DirectBoot.kt",
        ).readText()
        assertTrue(manager.contains("protector.openLegacyToken(envelope)"))
        assertTrue(manager.contains("preferences.commit { putString(\"token\", sealed) }"))
        assertTrue(directBoot.contains("preferences.edit().apply(block).commit()"))
        assertTrue(manager.contains("covalent_embedded_node_credentials"))
        assertFalse(
            "The local migration owner must not import or instantiate the external credential store",
            manager.contains("SecureNodeStore"),
        )
    }

    private fun rustJniSource(): String = repositoryFile("crates/covalent-android-jni/src/lib.rs")
        .readText()

    private companion object {
        fun moduleFile(relative: String): File {
            val direct = File(relative)
            if (direct.isFile) return direct
            var candidate: File? = File("").absoluteFile
            while (candidate != null) {
                val resolved = File(candidate, "apps/android/app/$relative")
                if (resolved.isFile) return resolved
                candidate = candidate.parentFile
            }
            error("Unable to locate $relative from ${File("").absolutePath}")
        }

        fun repositoryFile(relative: String): File {
            var candidate: File? = File("").absoluteFile
            while (candidate != null) {
                val resolved = File(candidate, relative)
                if (resolved.isFile) return resolved
                candidate = candidate.parentFile
            }
            error("Unable to locate $relative from ${File("").absolutePath}")
        }
    }
}
