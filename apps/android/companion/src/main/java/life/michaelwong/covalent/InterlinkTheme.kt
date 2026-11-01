package life.michaelwong.covalent

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

private val ForestLight = lightColorScheme(
    primary = Color(0xFF365E3F), onPrimary = Color.White,
    primaryContainer = Color(0xFFB8F2B9), onPrimaryContainer = Color(0xFF002108),
    secondary = Color(0xFF52634F), onSecondary = Color.White,
    secondaryContainer = Color(0xFFD5E8CE), onSecondaryContainer = Color(0xFF101F0F),
    tertiary = Color(0xFF38656A), onTertiary = Color.White,
    tertiaryContainer = Color(0xFFBCEBF0), onTertiaryContainer = Color(0xFF002024),
    background = Color(0xFFF8FBF1), onBackground = Color(0xFF191D17),
    surface = Color(0xFFF8FBF1), onSurface = Color(0xFF191D17),
    surfaceVariant = Color(0xFFDEE5D8), onSurfaceVariant = Color(0xFF42493F),
    outline = Color(0xFF72796E), outlineVariant = Color(0xFFC2C9BC),
    surfaceContainerLowest = Color.White, surfaceContainerLow = Color(0xFFF2F5EB),
    surfaceContainer = Color(0xFFECEFE5), surfaceContainerHigh = Color(0xFFE6E9DF),
    surfaceContainerHighest = Color(0xFFE0E4DA),
    inverseSurface = Color(0xFF2D322B), inverseOnSurface = Color(0xFFF0F3E9),
    inversePrimary = Color(0xFF9DD59F), surfaceTint = Color(0xFF365E3F),
)

private val ForestDark = darkColorScheme(
    primary = Color(0xFF9DD59F), onPrimary = Color(0xFF053913),
    primaryContainer = Color(0xFF214F29), onPrimaryContainer = Color(0xFFB8F2B9),
    secondary = Color(0xFFB9CCB2), onSecondary = Color(0xFF253423),
    secondaryContainer = Color(0xFF3B4B38), onSecondaryContainer = Color(0xFFD5E8CE),
    tertiary = Color(0xFFA0CFD4), onTertiary = Color(0xFF00363B),
    tertiaryContainer = Color(0xFF1F4D52), onTertiaryContainer = Color(0xFFBCEBF0),
    background = Color(0xFF10150F), onBackground = Color(0xFFE0E4DA),
    surface = Color(0xFF10150F), onSurface = Color(0xFFE0E4DA),
    surfaceVariant = Color(0xFF42493F), onSurfaceVariant = Color(0xFFC2C9BC),
    outline = Color(0xFF8C9387), outlineVariant = Color(0xFF42493F),
    surfaceContainerLowest = Color(0xFF0B100A), surfaceContainerLow = Color(0xFF191D17),
    surfaceContainer = Color(0xFF1D211B), surfaceContainerHigh = Color(0xFF272C25),
    surfaceContainerHighest = Color(0xFF32372F),
    inverseSurface = Color(0xFFE0E4DA), inverseOnSurface = Color(0xFF2D322B),
    inversePrimary = Color(0xFF365E3F), surfaceTint = Color(0xFF9DD59F),
)

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
internal fun InterlinkTheme(content: @Composable () -> Unit) {
    val context = LocalContext.current
    val dark = isSystemInDarkTheme()
    val colors = when {
        Build.VERSION.SDK_INT >= 31 && dark -> dynamicDarkColorScheme(context)
        Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(context)
        dark -> ForestDark
        else -> ForestLight
    }
    MaterialExpressiveTheme(colorScheme = colors, motionScheme = MotionScheme.expressive(), content = content)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun InterlinkScaffold(content: @Composable (PaddingValues) -> Unit) {
    Scaffold(
        containerColor = MaterialTheme.colorScheme.surface,
        topBar = {
            TopAppBar(
                title = {
                    Text(stringResource(R.string.app_name), fontFamily = FontFamily.Serif,
                        style = MaterialTheme.typography.headlineMedium,
                        maxLines = 1, overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.semantics { heading() })
                },
                navigationIcon = {
                    Icon(painterResource(R.drawable.ic_interlink_mark), contentDescription = null,
                        modifier = Modifier.padding(start = 20.dp, end = 12.dp).size(28.dp))
                },
            )
        },
        content = content,
    )
}
