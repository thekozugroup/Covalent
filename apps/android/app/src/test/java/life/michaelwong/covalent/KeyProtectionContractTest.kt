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
            "nativeStart's JNI descriptor must carry the trailing int protection level",
            rust.contains("\"(Ljava/lang/String;Ljava/lang/String;Z[BJJI)Ljava/lang/String;\""),
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
            "The descriptor (String, String, boolean, byte[], long, long, int) has seven " +
                "parameters; Kotlin declares ${parameters.size}: $parameters",
            7,
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
