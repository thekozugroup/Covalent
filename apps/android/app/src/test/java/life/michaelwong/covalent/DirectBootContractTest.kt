package life.michaelwong.covalent

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Android's Direct Boot boundary, pinned against the source.
 *
 * Between a reboot and the user's first unlock only *device-protected* storage is
 * readable. `Context.getSharedPreferences` is credential-encrypted by default and does not
 * return an empty file in that window — it throws:
 *
 * ```
 * IllegalStateException: SharedPreferences in credential encrypted storage
 * are not available until after user (id 0) is unlocked
 * ```
 *
 * The crash this fixture guards was `Application.onCreate` constructing
 * `EmbeddedNodeManager`, whose constructor opens a preference file. Every process start in
 * the locked window — an unattended reboot, an instrumentation run, a scheduled backup
 * waking the app — took the whole process down.
 *
 * These assertions are written against the **source text** rather than against behaviour
 * on the test JVM for two reasons. There is no Android framework here to lock, and the
 * failure mode is a *reintroduction*: someone adds one more preference read to a
 * constructor that runs at process start, which changes no signature and breaks no other
 * test. [life.michaelwong.covalent.node.DirectBootStorageTest] covers the behaviour on a
 * real device; this covers the shape that keeps the behaviour reachable.
 */
class DirectBootContractTest {

    /**
     * The specific regression: `Application.onCreate` runs on every process start,
     * including starts that happen before the user has ever unlocked. Nothing it calls may
     * touch credential-encrypted storage.
     */
    @Test
    fun applicationStartupReadsNoCredentialEncryptedStorage() {
        val application = source("CovalentApplication.kt")
        val onCreate = application
            .substringAfter("override fun onCreate()")
            .substringBefore("override val workManagerConfiguration")
        assertFalse(
            "Application.onCreate must not open SharedPreferences: credential-encrypted " +
                "storage throws until the user's first unlock, which kills the process.",
            onCreate.contains("getSharedPreferences"),
        )
        assertFalse(
            "EmbeddedNodeManager opens a credential-encrypted preference file in its " +
                "constructor, so Application.onCreate must not construct it directly. " +
                "Defer it with DirectBoot.whenUserUnlocked.",
            Regex("EmbeddedNodeManager\\(this\\)\\s*\\.\\s*reconnectIfEnabled\\(\\)")
                .containsMatchIn(onCreate.substringBefore("whenUserUnlocked")),
        )
        assertTrue(
            "Application.onCreate must route embedded-node startup through the " +
                "Direct Boot gate so it waits for the user's first unlock.",
            onCreate.contains("DirectBoot.whenUserUnlocked"),
        )
    }

    /**
     * The gate has to be the only way in, or the next preference read added to
     * [life.michaelwong.covalent.node.EmbeddedNodeManager] reintroduces the crash.
     */
    @Test
    fun theEmbeddedNodeOpensCredentialStorageOnlyThroughTheDirectBootGate() {
        val manager = source("node/EmbeddedNodeManager.kt")
        assertEquals(
            "EmbeddedNodeManager and its credential store must open preferences through " +
                "CredentialProtectedPreferences, never Context.getSharedPreferences, so a " +
                "sealed credential-encrypted volume reports \"locked\" instead of throwing.",
            0,
            Regex("\\.getSharedPreferences\\(").findAll(manager).count(),
        )
        assertTrue(
            "The provider preference file must go through the gate",
            manager.contains("CredentialProtectedPreferences(applicationContext, PREFERENCES_NAME)"),
        )
        assertTrue(
            "The local credential file must go through the gate too: it holds the sealed " +
                "API token envelope and is opened from the same constructor.",
            manager.contains("CredentialProtectedPreferences(context, \"covalent_embedded_node_credentials\")"),
        )
        assertTrue(
            "A locked device must fail closed on key protection rather than probing the " +
                "Keystore and minting the identity key in a boot state nothing else covers.",
            manager.contains("if (!preferences.readable) KeyProtectionLevel.UNAVAILABLE"),
        )
        assertTrue(
            "Starting the node while storage is sealed must refuse with the waiting-for-" +
                "unlock reason, not with the permanent \"cannot protect its identity\" copy.",
            manager.contains("return unavailable(LOCKED_STORAGE_MESSAGE)"),
        )
    }

    /**
     * A scheduled backup is exactly the thing that fires while a phone sits locked. It has
     * to *defer* — crashing loses the process and silently skipping loses the backup.
     */
    @Test
    fun aScheduledTransferDefersWhileCredentialStorageIsSealed() {
        val execution = source("work/TransferExecution.kt")
        assertTrue(
            "TransferExecution must consult the Direct Boot gate before it opens the " +
                "encrypted transfer store.",
            execution.contains("DirectBoot.isUserUnlocked(context)"),
        )
        val gate = execution.substringAfter("private fun openStoreOrDefer")
        assertTrue(
            "A sealed volume must produce a deferral, never a crash and never a silent skip",
            gate.contains("TransferOutcome.RETRY") || execution.contains("?: return TransferOutcome.RETRY"),
        )
        assertTrue(
            "Only the IllegalStateException the platform raises for sealed storage may be " +
                "turned into a deferral; anything else must keep propagating.",
            gate.contains("if (failure !is IllegalStateException) throw failure"),
        )
        assertTrue(
            "The store must be opened through the deferring helper, not constructed inline",
            execution.contains("val store = openStoreOrDefer(context, jobId) ?: return TransferOutcome.RETRY"),
        )
    }

    /**
     * The security half of the fix.
     *
     * Device-protected storage is readable by anyone holding a *locked* phone, so it must
     * never hold the local API token envelope, the node's TLS identity, the address of a
     * paired node — or the flag that says this phone is a Covalent backup target, which is
     * an inventory signal handed over for no functional gain. The node's state directory is
     * `noBackupFilesDir` and its token envelope is a credential-encrypted preference file,
     * so nothing can start before unlock however many flags move. Nothing moves.
     */
    @Test
    fun noCovalentStateIsMovedToDeviceProtectedStorage() {
        val offenders = kotlinSources()
            .filter { withoutComments(it.readText()).contains("createDeviceProtectedStorageContext") }
            .map { it.name }
            .sorted()
            .toList()
        assertEquals(
            "Device-protected storage is readable while the phone is locked. Covalent " +
                "keeps nothing there: the node cannot run before unlock in any case, and a " +
                "readable \"enabled\" flag would tell a locked-phone attacker this device " +
                "holds a backup set. Offending files: $offenders",
            emptyList<String>(),
            offenders,
        )
        assertTrue(
            "The reason for that decision must stay recorded next to the gate itself",
            source("node/DirectBoot.kt").contains("createDeviceProtectedStorageContext"),
        )
    }

    private fun source(relative: String): String =
        moduleFile("src/main/java/life/michaelwong/covalent/$relative").readText()

    private fun kotlinSources(): Sequence<File> =
        checkNotNull(moduleFile("src/main/java/life/michaelwong/covalent/CovalentApplication.kt").parentFile)
            .walkTopDown()
            .filter { it.isFile && it.extension == "kt" }

    /**
     * The decision not to use device-protected storage has to be *written down* somewhere,
     * and the natural place is next to the gate that implements it. Stripping comments
     * before the scan lets that explanation exist without registering as a use of the API
     * it is explaining.
     */
    private fun withoutComments(source: String): String = source
        .replace(Regex("/\\*.*?\\*/", RegexOption.DOT_MATCHES_ALL), "")
        .replace(Regex("//[^\n]*"), "")

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
    }
}
