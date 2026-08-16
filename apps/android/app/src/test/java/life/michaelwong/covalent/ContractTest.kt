package life.michaelwong.covalent

import java.io.File
import life.michaelwong.covalent.model.NodeEventKind
import life.michaelwong.covalent.model.PlatformTier
import life.michaelwong.covalent.model.PrimaryAction
import life.michaelwong.covalent.model.TransferKind
import life.michaelwong.covalent.model.TransferState
import life.michaelwong.covalent.ui.Screen
import life.michaelwong.covalent.ui.pairingInvitationKeyboardOptions
import life.michaelwong.covalent.ui.shouldReturnToSetupAfterRefreshFailure
import life.michaelwong.covalent.ui.startupRefreshDispatcher
import life.michaelwong.covalent.ui.systemBackTarget
import life.michaelwong.covalent.data.NodeApiException
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import life.michaelwong.covalent.work.TransferExecutionModel
import life.michaelwong.covalent.work.TransferScheduler
import life.michaelwong.covalent.work.transferExecutionModel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertSame
import org.junit.Test

class ContractTest {
    @Test
    fun tierOneIsReleaseBlockingAndroidPolicy() {
        assertEquals("Tier 1", PlatformTier.TIER_1.label)
    }

    @Test
    fun primaryToolbarHasOnlyLockedScopeActions() {
        assertEquals(listOf("PAIR", "BACKUP", "RESTORE"), PrimaryAction.entries.map { it.name })
        assertFalse(PrimaryAction.entries.any { it.name.contains("SYNC") })
    }

    @Test
    fun api37UsesUserInitiatedJobInsteadOfForegroundWorker() {
        assertEquals(TransferExecutionModel.USER_INITIATED_JOB, transferExecutionModel(37))
        assertEquals(TransferExecutionModel.USER_INITIATED_JOB, transferExecutionModel(34))
        assertEquals(TransferExecutionModel.LEGACY_WORK_MANAGER, transferExecutionModel(33))
        assertEquals(
            TransferScheduler.stableSchedulerId("backup-stable-id"),
            TransferScheduler.stableSchedulerId("backup-stable-id"),
        )
    }

    @Test
    fun systemBackReturnsEverySecondaryScreenHome() {
        listOf(Screen.PAIR, Screen.BACKUP, Screen.RESTORE, Screen.SETTINGS).forEach {
            assertEquals(Screen.HOME, it.systemBackTarget())
        }
        assertEquals(null, Screen.HOME.systemBackTarget())
        assertEquals(null, Screen.SETUP.systemBackTarget())
    }

    @Test
    fun pairingInvitationUsesLiteralAsciiInput() {
        assertEquals(KeyboardCapitalization.None, pairingInvitationKeyboardOptions.capitalization)
        assertEquals(false, pairingInvitationKeyboardOptions.autoCorrectEnabled)
        assertEquals(KeyboardType.Password, pairingInvitationKeyboardOptions.keyboardType)
        assertEquals(ImeAction.Done, pairingInvitationKeyboardOptions.imeAction)
    }

    @Test
    fun authenticationFailureReturnsToSetupButOfflineFailureDoesNot() {
        assertTrue(shouldReturnToSetupAfterRefreshFailure(NodeApiException(401, 1, "unauthorized", false, "No")))
        assertFalse(shouldReturnToSetupAfterRefreshFailure(java.io.IOException("offline")))
    }

    @Test
    fun startupRefreshNeverRunsOnTheUiDispatcher() {
        assertSame(kotlinx.coroutines.Dispatchers.IO, startupRefreshDispatcher())
    }

    @Test
    fun sharedGoldenContractsMatchAndroidEnumsAndProtocol() {
        val error = fixture("error-v1.json").readText()
        assertEquals(1L, integerValue(error, "protocolVersion"))
        assertEquals("source_changed", stringValue(error, "code"))
        assertTrue(booleanValue(error, "retryable"))

        val progress = fixture("progress-v1.json").readText()
        assertEquals(
            TransferKind.BACKUP,
            TransferKind.valueOf(stringValue(progress, "kind").uppercase()),
        )
        assertEquals(
            TransferState.RUNNING,
            TransferState.valueOf(stringValue(progress, "state").uppercase()),
        )

        val event = fixture("event-v1.json").readText()
        assertEquals(
            NodeEventKind.TRANSFER_CHANGED,
            NodeEventKind.valueOf(stringValue(event, "kind").uppercase()),
        )
        assertEquals(17L, integerValue(event, "sequence"))

        val backup = fixture("backup-summary-v1.json").readText()
        assertEquals(3L, integerValue(backup, "snapshotCount"))
        assertTrue(backup.contains("33333333-3333-4333-8333-333333333333"))
    }

    private fun stringValue(json: String, key: String): String = Regex(
        "\\\"${Regex.escape(key)}\\\"\\s*:\\s*\\\"([^\\\"]+)\\\"",
    ).find(json)?.groupValues?.get(1) ?: error("Missing string field $key")

    private fun integerValue(json: String, key: String): Long = Regex(
        "\\\"${Regex.escape(key)}\\\"\\s*:\\s*([0-9]+)",
    ).find(json)?.groupValues?.get(1)?.toLong() ?: error("Missing integer field $key")

    private fun booleanValue(json: String, key: String): Boolean = Regex(
        "\\\"${Regex.escape(key)}\\\"\\s*:\\s*(true|false)",
    ).find(json)?.groupValues?.get(1)?.toBooleanStrict() ?: error("Missing Boolean field $key")

    private fun fixture(name: String): File = generateSequence(
        File(System.getProperty("user.dir") ?: error("JVM working directory is unavailable")),
    ) {
        it.parentFile
    }.map { File(it, "fixtures/contracts/$name") }
        .firstOrNull(File::isFile)
        ?: error("Could not locate shared contract fixture $name")
}
