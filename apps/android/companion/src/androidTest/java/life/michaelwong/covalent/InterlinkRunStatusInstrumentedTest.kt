package life.michaelwong.covalent

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.model.*
import life.michaelwong.covalent.ui.FolderShareCard
import org.junit.Rule
import org.junit.Test

/** A saved source-owned run stays unchanged while its receiving UI admits uncertainty. */
class InterlinkRunStatusInstrumentedTest {
    @get:Rule val compose = createComposeRule()

    @Test fun disconnectedExpiredRunShowsConfirmationInsteadOfRunningOrCompleted() {
        val resources = InstrumentationRegistry.getInstrumentation().targetContext.resources
        val share = FolderShare("example-offer", "example-folder", "Pictures", "example-source", true,
            FolderSharePhase.READY, null, false, peerConnection = PeerConnectionState.DISCONNECTED,
            linkPolicy = FolderLinkPolicy(),
            linkSettings = FolderLinkSettingsState(0,
                FolderLinkSettings(FolderLinkPolicy(), false, FolderLinkCadence.Manual),
                "example-change", "example-source", true, null, null),
            linkRun = FolderLinkRunSummary(1, 1, 0, FolderLinkRunPhase.RUNNING, 1, 2, null, null, null, null, emptyList()))
        val status = FolderSyncStatus(FolderSyncAvailability.AVAILABLE, FolderSyncLifecycle.RUNNING, null,
            FolderHealthFreshness.FRESH, listOf(FolderSyncPeer("example-source", "Example source")), listOf(share), emptyList(),
            connectionFreshness = PeerConnectionFreshness.FRESH)
        val current = mutableStateOf(status)
        compose.setContent {
            InterlinkTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    FolderShareCard(status = current.value, share = share, peerName = "Example source",
                        hasFolder = true, hasPendingRepair = false, busy = false,
                        addDestination = {}, showLinkSettings = true,
                        savedSettingsChange = null, savedRunRequest = null,
                        editSettings = { _, _ -> }, retrySettings = {}, reviewSettings = { _, _ -> },
                        mutate = {}, runNow = {}, reviewRun = {}, localFolderName = "Pictures")
                }
            }
        }
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_awaiting_confirmation))
            .performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_awaiting_confirmation_detail))
            .performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_running)).assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.folder_sync_connection_unknown)).assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_succeeded)).assertDoesNotExist()
        compose.runOnIdle {
            current.value = status.copy(folders = listOf(FolderHealth("example-folder", "syncing", 0, 0, 0, 0, false, false)))
        }
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_awaiting_confirmation)).assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_running)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.folder_link_run_succeeded)).assertDoesNotExist()
    }
}
