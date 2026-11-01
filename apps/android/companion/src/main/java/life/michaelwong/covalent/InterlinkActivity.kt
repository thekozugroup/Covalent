package life.michaelwong.covalent

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import life.michaelwong.covalent.ui.FolderSyncScreen

/** Focused photo companion; no backup-server enrollment or recovery surface. */
class InterlinkActivity : ComponentActivity() {
    @OptIn(ExperimentalMaterial3ExpressiveApi::class, ExperimentalMaterial3Api::class)
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val owner = application as InterlinkApplication
        setContent {
            val manager by owner.manager.collectAsState()
            val failed by owner.failed.collectAsState()
            InterlinkTheme {
                InterlinkScaffold { padding ->
                    Box(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding).imePadding()) {
                        val ready = manager
                        if (ready != null) {
                            FolderSyncScreen(
                                manager = ready,
                                photoAppliance = true,
                                deviceHeader = {
                                    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) { InterlinkApplianceDashboard(ready) }
                                },
                                modifier = Modifier.widthIn(max = 840.dp).fillMaxHeight()
                                    .align(androidx.compose.ui.Alignment.TopCenter),
                            )
                        } else {
                            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp),
                                horizontalAlignment = androidx.compose.ui.Alignment.CenterHorizontally,
                                verticalArrangement = Arrangement.spacedBy(16.dp, androidx.compose.ui.Alignment.CenterVertically)) {
                                if (failed) {
                                    Text("Secure sync could not start. Your saved identity is retained.")
                                    Button(onClick = owner::reconnect) { Text("Retry") }
                                } else {
                                    LoadingIndicator()
                                    Text("Preparing secure sync…")
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    override fun onResume() {
        super.onResume()
        (application as InterlinkApplication).reconnect()
    }
}
