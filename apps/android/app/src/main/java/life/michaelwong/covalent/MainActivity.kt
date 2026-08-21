package life.michaelwong.covalent

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import life.michaelwong.covalent.ui.CovalentApp
import life.michaelwong.covalent.ui.CovalentViewModel
import life.michaelwong.covalent.ui.theme.CovalentTheme

class MainActivity : ComponentActivity() {
    private val covalentViewModel: CovalentViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        readSetupLink(intent)
        setContent {
            CovalentTheme {
                CovalentApp(stateOverride = covalentViewModel)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        readSetupLink(intent)
    }

    /**
     * Records a `covalent://connect?endpoint=…` link opened from another app. The link is
     * untrusted input: the UI validates it and, at most, prefills the server address field.
     * No token is ever accepted from a link and no connection is made without confirmation.
     */
    private fun readSetupLink(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        val link = intent.data?.toString().orEmpty()
        if (link.isNotBlank()) covalentViewModel.pendingSetupLink = link
    }
}
