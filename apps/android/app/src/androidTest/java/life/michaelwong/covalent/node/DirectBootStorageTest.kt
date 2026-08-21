package life.michaelwong.covalent.node

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.os.UserManager
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.work.TransferExecution
import life.michaelwong.covalent.work.TransferOutcome
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.After
import org.junit.Test

/**
 * Direct Boot: the app must survive a process start on a locked device.
 *
 * Between a reboot and the user's first unlock, Android seals credential-encrypted
 * storage. `Context.getSharedPreferences` does not return an empty file then — it throws
 * `IllegalStateException: SharedPreferences in credential encrypted storage are not
 * available until after user (id 0) is unlocked`. That is the exact crash that took the
 * process down from `CovalentApplication.onCreate`.
 *
 * ## Why a context double rather than a really-locked device
 *
 * Reaching the pre-first-unlock state for real needs a lockscreen credential plus a reboot,
 * and the window closes the moment anything unlocks the device — it cannot be entered and
 * left inside a test method, and an instrumentation run that reaches this class has by
 * definition already got the app running. [LockedUserContext] therefore stands in for the
 * platform by throwing the platform's own exception from the platform's own method. That
 * is faithful, because the exception *is* the whole of the behaviour under test: there is
 * no state the app can observe in that window other than "this call throws".
 *
 * [theRealDeviceReportsItsOwnUnlockState] keeps the double honest by checking the same
 * assertions against the real platform on the device the tests run on.
 */
class DirectBootStorageTest {
    private val realContext: Context = InstrumentationRegistry.getInstrumentation().targetContext
    private val lockedContext: Context = LockedUserContext(realContext)

    /**
     * `EmbeddedProviderState` is published through one process-wide flow, so a manager
     * built on the locked double leaves the fail-closed state behind for whatever runs
     * next. Rebuilding on the real context republishes the persisted state.
     */
    @After
    fun restorePublishedProviderState() {
        EmbeddedNodeManager(realContext)
    }

    /**
     * The regression itself. Before the fix this threw out of the constructor, and from
     * `Application.onCreate` that is a process kill on every launch while locked.
     */
    @Test
    fun theEmbeddedNodeManagerConstructsWhileCredentialStorageIsSealed() {
        val manager = EmbeddedNodeManager(lockedContext)
        assertNotNull("Constructing the manager on a locked device must not throw", manager)
    }

    /** Fails closed, and says which kind of "no" it is. */
    @Test
    fun theEmbeddedNodeRefusesToStartWhileCredentialStorageIsSealed() {
        val manager = EmbeddedNodeManager(lockedContext)

        assertEquals(
            "A sealed volume must report UNAVAILABLE rather than probing the Keystore and " +
                "minting the identity key during Direct Boot",
            KeyProtectionLevel.UNAVAILABLE,
            manager.keyProtectionLevel(),
        )
        assertFalse(
            "Admission must stay fail-closed while storage is sealed",
            manager.keyProtectionAvailable(),
        )
        assertEquals(
            "A sealed volume must not be mistaken for a configured local node",
            NodeMode.EXTERNAL,
            manager.activeMode(),
        )
        assertNull(
            "No local credential can be read from sealed storage, so none may be handed out",
            manager.localConnectionForActiveMode(),
        )
        assertFalse(
            "Local mode must not be selectable from sealed storage",
            manager.selectLocalMode(),
        )

        val start = manager.serviceStart()
        assertFalse("Starting the node from sealed storage must fail", start.ok)
        assertEquals("stopped", start.state)
        assertEquals(
            "A sealed volume must not be reported as \"the user turned this off\"",
            "embedded_provider_unavailable",
            start.code,
        )
        assertTrue(
            "The refusal must say the wait ends at unlock, not that the phone can never " +
                "protect its identity: ${start.message}",
            start.message.contains("unlock", ignoreCase = true),
        )
        assertFalse(
            "A locked phone is not a phone that cannot protect its identity: ${start.message}",
            start.message.contains("cannot protect", ignoreCase = true),
        )
    }

    /**
     * Enabling is the path a person drives, but it is also reachable from a restored
     * process, so it gets the same wait-for-unlock answer instead of the permanent one.
     */
    @Test
    fun enablingWhileCredentialStorageIsSealedRefusesWithTheWaitingReason() {
        val manager = EmbeddedNodeManager(lockedContext)
        manager.enable(maxBytes = 2L * 1024L * 1024L * 1024L, keepFreeBytes = 512L * 1024L * 1024L)

        val state = manager.state.value
        assertFalse("Sealed storage must never enable the provider", state.enabled)
        assertFalse("Sealed storage must never mark the provider running", state.running)
        assertFalse("Sealed storage must never report the provider supported", state.supported)
        assertEquals(KeyProtectionLevel.UNAVAILABLE, state.keyProtectionLevel)
        assertTrue(
            "The person must be told the wait ends at unlock: ${state.statusMessage}",
            state.statusMessage.contains("unlock", ignoreCase = true),
        )
    }

    /**
     * A scheduled backup is exactly what fires while a phone sits locked. Deferring is the
     * correct outcome; crashing loses the process and a silent success loses the backup.
     */
    @Test
    fun aScheduledTransferDefersWhileCredentialStorageIsSealed() {
        assertEquals(
            "A transfer that cannot read its own encrypted store must be rescheduled, not " +
                "crashed and not quietly dropped",
            TransferOutcome.RETRY,
            TransferExecution.run(lockedContext, "covalent-direct-boot-probe"),
        )
    }

    /**
     * The double must not be describing a device that does not exist. On the real device
     * the same calls go through the real platform, and the real gate must agree with the
     * real `UserManager`.
     */
    @Test
    fun theRealDeviceReportsItsOwnUnlockState() {
        val userManager = realContext.getSystemService(UserManager::class.java)
        assertNotNull("Android must expose UserManager", userManager)
        assertEquals(
            "DirectBoot must report exactly what the platform reports",
            userManager!!.isUserUnlocked,
            DirectBoot.isUserUnlocked(realContext),
        )
        assertTrue(
            "The instrumentation host is unlocked, so credential-encrypted storage must be " +
                "readable and the fix must not have made the normal path report otherwise",
            DirectBoot.isUserUnlocked(realContext),
        )

        // And the gate must be transparent once unlocked: the manager built on the real
        // context measures the Keystore instead of returning the fail-closed answer.
        val manager = EmbeddedNodeManager(realContext)
        assertEquals(
            "Once unlocked, key protection must be the measured level, not the Direct Boot " +
                "refusal",
            IdentityKeyProtector().protection(),
            manager.keyProtectionLevel(),
        )
    }

    /**
     * A context that behaves the way Android does before this user's first unlock:
     * credential-encrypted `SharedPreferences` throw instead of opening, with the
     * platform's own message.
     */
    private class LockedUserContext(base: Context) : ContextWrapper(base) {
        override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
            throw IllegalStateException(
                "SharedPreferences in credential encrypted storage are not available " +
                    "until after user (id 0) is unlocked",
            )

        override fun getApplicationContext(): Context = this

        override fun createDeviceProtectedStorageContext(): Context =
            throw AssertionError(
                "Covalent must not reach for device-protected storage: it is readable " +
                    "from a locked phone and must never hold node state.",
            )
    }
}
