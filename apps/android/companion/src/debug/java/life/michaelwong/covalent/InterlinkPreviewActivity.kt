package life.michaelwong.covalent

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import life.michaelwong.covalent.model.*
import life.michaelwong.covalent.ui.FolderShareCard
import life.michaelwong.covalent.ui.FolderSyncOverview
import life.michaelwong.covalent.appliance.*

/** Debug-only visual fixture. Every value is synthetic; callbacks change no state. */
class InterlinkPreviewActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val link = FolderShare(
            offerId = "preview-link", folderId = "preview-pictures", label = "Pictures",
            peerId = "preview-pi", incoming = false, phase = FolderSharePhase.READY,
            expiresAtUnixMs = null, expired = false, linkPolicy = FolderLinkPolicy(),
        )
        val status = FolderSyncStatus(
            availability = FolderSyncAvailability.AVAILABLE, lifecycle = FolderSyncLifecycle.STOPPED,
            issue = null, healthFreshness = FolderHealthFreshness.NEVER_OBSERVED,
            peers = listOf(FolderSyncPeer("preview-pi", "Living room Pi")),
            shares = listOf(link), folders = emptyList(),
        )
        val phone = ApplianceIdentity("11111111-1111-4111-8111-111111111111", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE", "b".repeat(20))
        val pi = ApplianceIdentity("22222222-2222-4222-8222-222222222222", "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI", "a".repeat(20))
        val selected = SelectedAppliance(phone, pi, "Living room Pi", "44444444-4444-4444-8444-444444444444")
        val previewMode = intent.getStringExtra("previewMode")
        val dashboard = ApplianceDashboardState(
            selection = selected,
            connection = when (previewMode) {
                "offline" -> ApplianceConnectionState.UNAVAILABLE
                "approval" -> ApplianceConnectionState.APPROVAL_REQUIRED
                else -> ApplianceConnectionState.CONNECTED
            },
            context = ApplianceContext(phone, listOf(pi)), candidates = listOf(ApplianceCandidate(pi, selected.name)),
            observation = if (previewMode in setOf("offline", "approval")) null else
                ApplianceObservation(selected.confirmedGeneration!!, ApplianceSyncState.RUNNING, 1, true),
        )
        setContent {
            InterlinkTheme {
                InterlinkScaffold { padding ->
                    Column(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding)
                        .verticalScroll(rememberScrollState()).padding(20.dp, 14.dp),
                        verticalArrangement = Arrangement.spacedBy(14.dp)) {
                        Text("Example data · visual preview", style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant)
                        InterlinkApplianceCard(dashboard, false, {}, {})
                        Text("Your photo links", style = MaterialTheme.typography.headlineMedium,
                            fontWeight = FontWeight.SemiBold)
                        Text(stringResource(R.string.interlink_subtitle))
                        FolderSyncOverview(status, true, emptySet(), false, {})
                        FolderShareCard(
                            status = status, share = link, peerName = "Living room Pi",
                            hasFolder = true, hasPendingRepair = false, busy = false,
                            addDestination = {}, showLinkSettings = false,
                            savedSettingsChange = null, savedRunRequest = null,
                            editSettings = { _, _ -> }, retrySettings = {}, reviewSettings = { _, _ -> },
                            mutate = {}, runNow = {}, reviewRun = {}, localFolderName = "Pictures",
                        )
                        Text("Example data · visual preview", style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
    }
}
