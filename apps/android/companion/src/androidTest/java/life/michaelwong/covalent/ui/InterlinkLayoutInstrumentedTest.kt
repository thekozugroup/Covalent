package life.michaelwong.covalent.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.R
import life.michaelwong.covalent.model.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** Synthetic view states only: no account, pairing, folder grant or media writes. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
class InterlinkLayoutInstrumentedTest {
    @get:Rule val compose = createComposeRule()
    private val resources = InstrumentationRegistry.getInstrumentation().targetContext.resources

    @Test
    fun idleSavedLinkKeepsSetupWhileAttentionAndDestinationStayActionable() {
        val link = savedLink()
        val status = mutableStateOf(snapshot(listOf(link)))
        val repairs = mutableStateOf(emptySet<String>())
        compose.setContent {
            MaterialExpressiveTheme {
                FolderSyncOverview(status.value, true, repairs.value, false, {})
            }
        }
        fun message(id: Int) = compose.onNodeWithTag("folder-sync-overview")
            .assertIsDisplayed().assertTextEquals(resources.getString(id))

        // Manual/paused links persist even when the native worker list is empty.
        message(R.string.interlink_links_ready)
        compose.runOnIdle {
            status.value = snapshot(listOf(link.copy(phase = FolderSharePhase.PAUSED),
                link.copy(offerId = "removed", phase = FolderSharePhase.REMOVED, remoteRemovalPending = true)))
        }
        message(R.string.interlink_links_paused)
        compose.runOnIdle { repairs.value = setOf(link.offerId) }
        message(R.string.folder_sync_connection_attention)
        compose.runOnIdle {
            repairs.value = emptySet()
            status.value = snapshot(listOf(link.copy(incoming = true, phase = FolderSharePhase.OFFERED)))
        }
        message(R.string.interlink_destination_next)
        compose.runOnIdle {
            status.value = snapshot(listOf(link.copy(incoming = true, phase = FolderSharePhase.OFFERED, expired = true)))
        }
        message(R.string.interlink_links_pending)
        compose.runOnIdle {
            status.value = snapshot(listOf(link.copy(phase = FolderSharePhase.REMOVED, remoteRemovalPending = true)))
        }
        message(R.string.interlink_links_removing)
        compose.runOnIdle { status.value = snapshot(emptyList()) }
        message(R.string.interlink_folder_next)
        compose.runOnIdle { status.value = snapshot(emptyList()).copy(peers = emptyList()) }
        message(R.string.interlink_pair_next)
        compose.runOnIdle {
            status.value = snapshot(listOf(link)).copy(lifecycle = FolderSyncLifecycle.NEEDS_ATTENTION)
        }
        message(R.string.folder_sync_connection_attention)
    }

    @Test
    fun largeTextStacksBothEndpointsAndKeepsRefreshReachable() {
        val narrow = mutableStateOf(true)
        val busy = mutableStateOf(false)
        val link = savedLink()
        var refreshes = 0
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, if (narrow.value) 2f else 1f)) {
                MaterialExpressiveTheme {
                    Column(Modifier.width(if (narrow.value) 280.dp else 480.dp).verticalScroll(rememberScrollState())) {
                        FolderSyncOverview(snapshot(listOf(link)), true, emptySet(), busy.value, { refreshes++ })
                        FolderShareCard(
                            status = snapshot(listOf(link)), share = link, peerName = "Living room photo host",
                            hasFolder = true, hasPendingRepair = false, busy = false, addDestination = {},
                            showLinkSettings = false, savedSettingsChange = null, savedRunRequest = null,
                            editSettings = { _, _ -> }, retrySettings = {}, reviewSettings = { _, _ -> },
                            mutate = {}, runNow = {}, reviewRun = {}, localFolderName = "Pictures / Family holidays",
                        )
                    }
                }
            }
        }
        val source = compose.onNodeWithTag("folder-link-source-${link.offerId}")
        val destination = compose.onNodeWithTag("folder-link-destination-${link.offerId}")
        source.performScrollTo().assertIsDisplayed()
        destination.performScrollTo().assertIsDisplayed()
        val sourceBounds = source.getUnclippedBoundsInRoot()
        val destinationBounds = destination.getUnclippedBoundsInRoot()
        assertTrue("Large text needs distinct full-width endpoints", destinationBounds.top >= sourceBounds.bottom)
        assertEquals(sourceBounds.left, destinationBounds.left)
        assertEquals(sourceBounds.right, destinationBounds.right)
        val refresh = compose.onNodeWithText(resources.getString(R.string.action_refresh))
        refresh.performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle { assertEquals(1, refreshes); busy.value = true }
        refresh.assertIsNotEnabled()
        compose.runOnIdle { narrow.value = false }
        val wideSource = source.getUnclippedBoundsInRoot()
        val wideDestination = destination.getUnclippedBoundsInRoot()
        assertTrue("Normal text on a wide card keeps adjacent endpoints", wideDestination.left >= wideSource.right)
        assertEquals(wideSource.top, wideDestination.top)
    }

    private fun savedLink() = FolderShare(
        offerId = "layout-only", folderId = "pictures", label = "Family photos", peerId = "host",
        incoming = false, phase = FolderSharePhase.READY, expiresAtUnixMs = null, expired = false,
        linkPolicy = FolderLinkPolicy(),
    )

    private fun snapshot(links: List<FolderShare>) = FolderSyncStatus(
        availability = FolderSyncAvailability.AVAILABLE, lifecycle = FolderSyncLifecycle.STOPPED,
        issue = null, healthFreshness = FolderHealthFreshness.NEVER_OBSERVED,
        peers = listOf(FolderSyncPeer("host", "Living room photo host")), shares = links, folders = emptyList(),
    )
}
