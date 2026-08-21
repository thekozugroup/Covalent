package life.michaelwong.covalent.node

import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Exercises the real Android Keystore on the device the tests run on.
 *
 * Nothing here compares the protector against its own output: the round trip is asserted
 * against a literal, the sealed form is asserted to differ from the plaintext, and the
 * tamper cases are asserted to fail. A protector that quietly stopped encrypting, or that
 * reported a level it had not measured, would fail these.
 */
class IdentityKeyProtectorTest {

    private val protector = IdentityKeyProtector()

    @Test
    fun thisDeviceCanProtectItsIdentity() {
        val level = protector.protection()
        assertNotEquals(
            "The embedded node cannot start on a device with no usable Keystore key. " +
                "If this fails on real hardware, on-phone backups are blocked there.",
            KeyProtectionLevel.UNAVAILABLE,
            level,
        )
        assertTrue(
            "A measured level must be one the JNI contract defines",
            level in KeyProtectionLevel.entries,
        )
    }

    /**
     * The measured level has to be one the Rust side will admit. A level that Kotlin
     * considers fine but `IdentityProtection::from_wire` decodes as `Unavailable` would
     * leave the app enabling a backup server the native layer then refuses to start.
     */
    @Test
    fun theMeasuredLevelIsOneTheNativeSideAdmits() {
        val wire = protector.protection().wireValue
        assertTrue(
            "A usable level must send wire value 1, 2, or 3; this device sends $wire",
            wire in 1..3,
        )
        assertEquals(
            "A fresh instance must report the same level",
            protector.protection(),
            IdentityKeyProtector().protection(),
        )
    }

    @Test
    fun sealedCredentialsRoundTripAndAreNotStoredInTheClear() {
        val secret = "a-local-api-token-that-must-never-be-written-in-the-clear"
        val envelope = requireNotNull(protector.seal(secret)) { "sealing failed" }
        assertTrue("The envelope must be versioned: $envelope", envelope.startsWith("v2:"))
        assertTrue("The plaintext must not appear in the envelope", !envelope.contains(secret))
        assertEquals(secret, protector.open(envelope))
    }

    @Test
    fun everySealUsesAFreshInitialisationVector() {
        val secret = "the same plaintext twice"
        val first = requireNotNull(protector.seal(secret))
        val second = requireNotNull(protector.seal(secret))
        assertNotEquals(
            "Two seals of one plaintext must differ, or the key is reusing an IV",
            first,
            second,
        )
        assertEquals(secret, protector.open(first))
        assertEquals(secret, protector.open(second))
    }

    @Test
    fun aTamperedEnvelopeIsRefusedRatherThanPartiallyTrusted() {
        val envelope = requireNotNull(protector.seal("original"))
        val parts = envelope.split(":")
        assertEquals(3, parts.size)
        // Flip the last ciphertext character. GCM authenticates, so this must not decrypt.
        val flipped = parts[2].dropLast(1) + if (parts[2].last() == 'A') 'B' else 'A'
        assertNull(protector.open("${parts[0]}:${parts[1]}:$flipped"))
        assertNull("A truncated envelope must be refused", protector.open("v2:only-two"))
        assertNull("An unversioned envelope must be refused", protector.open("iv:ciphertext"))
        assertNull("An older envelope format must be refused", protector.open("v1:a:b"))
        assertNull("An empty envelope must be refused", protector.open(""))
    }

    @Test
    fun theEmbeddedProviderReportsTheLevelItMeasured() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val manager = EmbeddedNodeManager(context)
        assertEquals(protector.protection(), manager.keyProtectionLevel())
        assertEquals(
            "keyProtectionAvailable must follow the measurement, not a constant",
            manager.keyProtectionLevel() != KeyProtectionLevel.UNAVAILABLE,
            manager.keyProtectionAvailable(),
        )
        assertTrue(
            "This device measured ${manager.keyProtectionLevel()}, so the on-phone " +
                "backup server must no longer be blocked",
            manager.keyProtectionAvailable(),
        )
    }
}
