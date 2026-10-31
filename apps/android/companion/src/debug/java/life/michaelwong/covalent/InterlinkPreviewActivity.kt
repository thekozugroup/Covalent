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
            peers = listOf(FolderSyncPeer("preview-pi", "Photo Pi")),
            shares = listOf(link), folders = emptyList(),
        )
        setContent {
            InterlinkTheme {
                InterlinkScaffold { padding ->
                    Column(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding)
                        .verticalScroll(rememberScrollState()).padding(20.dp, 14.dp),
                        verticalArrangement = Arrangement.spacedBy(14.dp)) {
                        Text("Your photo links", style = MaterialTheme.typography.headlineMedium,
                            fontWeight = FontWeight.SemiBold)
                        Text(stringResource(R.string.interlink_subtitle))
                        FolderSyncOverview(status, true, emptySet(), false, {})
                        FolderShareCard(
                            status = status, share = link, peerName = "Photo Pi",
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
