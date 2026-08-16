package life.michaelwong.covalent

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import life.michaelwong.covalent.ui.CovalentApp
import life.michaelwong.covalent.ui.theme.CovalentTheme
import org.junit.Rule
import org.junit.Test

class CovalentAppTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun foundationExplainsExplicitReplicaPolicy() {
        compose.setContent { CovalentTheme { CovalentApp() } }
        compose.onNodeWithText("Explicit selection").assertIsDisplayed()
    }
}
