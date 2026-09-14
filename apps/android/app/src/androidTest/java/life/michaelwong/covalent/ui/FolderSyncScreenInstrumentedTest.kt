package life.michaelwong.covalent.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.R
import life.michaelwong.covalent.model.AndroidLinkConditions
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderLinkCadence
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderLinkSettingsState
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.ui.theme.CovalentTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

class FolderSyncScreenInstrumentedTest {
    @get:Rule
    val compose = createComposeRule()

    private val resources = InstrumentationRegistry.getInstrumentation().targetContext.resources

    @Test
    fun savedRepairStaysVisibleWithoutRuntimeFolderStatus() {
        var mutation: String? = null
        val settings = FolderLinkSettings(
            deletionPolicy = FolderLinkPolicy(),
            paused = false,
            cadence = FolderLinkCadence.Manual,
            androidConditions = AndroidLinkConditions(),
        )
        val share = FolderShare(
            offerId = "offer-1",
            folderId = "folder-1",
            label = "Documents",
            peerId = "peer-1",
            incoming = true,
            phase = FolderSharePhase.READY,
            expiresAtUnixMs = null,
            expired = false,
            linkPolicy = settings.deletionPolicy,
            linkSettings = FolderLinkSettingsState(
                revision = 1,
                settings = settings,
                changeId = "change-1",
                changedBy = "peer-1",
                confirmed = true,
                pendingChange = null,
                conflictedChange = null,
            ),
        )
        val status = FolderSyncStatus(
            availability = FolderSyncAvailability.AVAILABLE,
            lifecycle = FolderSyncLifecycle.STOPPED,
            issue = null,
            healthFreshness = FolderHealthFreshness.NEVER_OBSERVED,
            peers = emptyList(),
            shares = listOf(share),
            folders = emptyList(),
        )

        compose.setContent {
            CovalentTheme {
                FolderShareCard(
                    status = status,
                    share = share,
                    peerName = "Tablet",
                    hasFolder = false,
                    hasPendingRepair = true,
                    busy = false,
                    addDestination = {},
                    showLinkSettings = false,
                    savedSettingsChange = null,
                    savedRunRequest = null,
                    editSettings = { _, _ -> },
                    retrySettings = {},
                    reviewSettings = { _, _ -> },
                    mutate = { mutation = it },
                    runNow = {},
                    reviewRun = {},
                )
            }
        }

        compose.onNodeWithText(resources.getString(R.string.folder_sync_connection_attention))
            .assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.folder_sync_saved_repair_detail))
            .assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.folder_sync_choose_again_detail))
            .assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_ready))
            .assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.folder_sync_retry_saved_repair))
            .assertIsDisplayed()
            .performClick()
        compose.runOnIdle { assertEquals("retry-repair", mutation) }
    }
}
