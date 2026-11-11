package life.michaelwong.covalent.ui

import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.R
import life.michaelwong.covalent.model.AndroidLinkConditions
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderLinkCadence
import life.michaelwong.covalent.model.FolderLinkRunDestinationSummary
import life.michaelwong.covalent.model.FolderLinkRunFailure
import life.michaelwong.covalent.model.FolderLinkRunFailureRole
import life.michaelwong.covalent.model.FolderLinkRunPhase
import life.michaelwong.covalent.model.FolderLinkRunResult
import life.michaelwong.covalent.model.FolderLinkRunSummary
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

    @Test
    fun aFailedCopyOnThisPhoneShowsItsCauseAndOtherwiseOnlyThePlainResult() {
        val settings = FolderLinkSettings(
            deletionPolicy = FolderLinkPolicy(),
            paused = false,
            cadence = FolderLinkCadence.Manual,
            androidConditions = AndroidLinkConditions(),
        )
        val share = FolderShare(
            offerId = "33333333-3333-4333-8333-333333333333",
            folderId = "11111111-1111-4111-8111-111111111111",
            label = "Camera",
            peerId = "22222222-2222-4222-8222-222222222222",
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
            linkRun = FolderLinkRunSummary(
                generation = 5,
                stateRevision = 1,
                settingsRevision = 1,
                phase = FolderLinkRunPhase.INCOMPLETE,
                startedAtUnixMs = 1,
                deadlineUnixMs = null,
                endedAtUnixMs = 2,
                nextDueAtUnixMs = null,
                pendingRequest = null,
                rejectedRequest = null,
                destinations = listOf(
                    FolderLinkRunDestinationSummary("44444444-4444-4444-8444-444444444444", FolderLinkRunResult.FAILED, 2),
                ),
            ),
        )
        val status = FolderSyncStatus(
            availability = FolderSyncAvailability.AVAILABLE,
            lifecycle = FolderSyncLifecycle.RUNNING,
            issue = null,
            healthFreshness = FolderHealthFreshness.NEVER_OBSERVED,
            peers = emptyList(),
            shares = listOf(share),
            folders = emptyList(),
        )
        val failure = mutableStateOf<FolderLinkRunFailure?>(
            FolderLinkRunFailure(share.folderId, 5, FolderLinkRunFailureRole.DESTINATION, "noSpace"),
        )
        compose.setContent {
            CovalentTheme {
                FolderShareCard(
                    status = status,
                    share = share,
                    peerName = "Studio Mac",
                    hasFolder = true,
                    hasPendingRepair = false,
                    busy = false,
                    addDestination = {},
                    showLinkSettings = true,
                    savedSettingsChange = null,
                    savedRunRequest = null,
                    editSettings = { _, _ -> },
                    retrySettings = {},
                    reviewSettings = { _, _ -> },
                    mutate = {},
                    runNow = {},
                    reviewRun = {},
                    runFailure = failure.value,
                )
            }
        }

        val failedRow = resources.getString(
            R.string.folder_link_run_destination,
            resources.getString(R.string.folder_link_this_phone),
            resources.getString(R.string.folder_link_run_result_failed),
        )
        val cause = resources.getString(R.string.folder_link_run_cause_no_space)
        compose.onNodeWithText(failedRow).assertExists()
        compose.onNodeWithText(cause).assertExists()
        // Without an explanation the card shows exactly what it showed before causes existed.
        compose.runOnIdle { failure.value = null }
        compose.onNodeWithText(failedRow).assertExists()
        compose.onNodeWithText(cause).assertDoesNotExist()
    }
}
