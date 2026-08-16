package life.michaelwong.covalent

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import life.michaelwong.covalent.ui.CovalentApp
import life.michaelwong.covalent.ui.theme.CovalentTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            CovalentTheme {
                CovalentApp()
            }
        }
    }
}
