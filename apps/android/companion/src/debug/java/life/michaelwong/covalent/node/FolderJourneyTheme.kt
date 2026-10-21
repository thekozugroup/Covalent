package life.michaelwong.covalent.node

import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.runtime.Composable

/** The dedicated native journey exercises its Expressive theme and motion. */
@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
internal fun FolderJourneyTheme(content: @Composable () -> Unit) {
    MaterialExpressiveTheme(motionScheme = MotionScheme.expressive(), content = content)
}
