package life.michaelwong.covalent.node

import androidx.compose.runtime.Composable
import life.michaelwong.covalent.ui.theme.CovalentTheme

/** Native journey uses this app's theme without changing its dependency graph. */
@Composable
internal fun FolderJourneyTheme(content: @Composable () -> Unit) {
    CovalentTheme(content = content)
}
