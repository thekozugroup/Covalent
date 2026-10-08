package life.michaelwong.covalent.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.Shapes
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val LightColors = lightColorScheme(
    primary = Color(0xFF475569),
    onPrimary = Color.White,
    secondary = Color(0xFF52525B),
    tertiary = Color(0xFF047857),
    background = Color(0xFFFAFAFA),
    surface = Color.White,
    surfaceContainer = Color(0xFFF4F4F5),
    surfaceContainerLow = Color.White,
    surfaceVariant = Color(0xFFF4F4F5),
    onBackground = Color(0xFF27272A),
    onSurface = Color(0xFF27272A),
    onSurfaceVariant = Color(0xFF52525B),
    outlineVariant = Color(0xFFE4E4E7),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFFCBD5E1),
    secondary = Color(0xFFD4D4D8),
    tertiary = Color(0xFFA1D0C1),
    background = Color(0xFF18181B),
    surface = Color(0xFF27272A),
    onSurface = Color(0xFFF4F4F5),
    surfaceContainer = Color(0xFF27272A),
    surfaceContainerLow = Color(0xFF27272A),
)

@Composable
fun CovalentTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = false,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val colors = when {
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && darkTheme -> dynamicDarkColorScheme(context)
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> dynamicLightColorScheme(context)
        darkTheme -> DarkColors
        else -> LightColors
    }

    val baseline = Typography()
    val typography = baseline.copy(
        headlineLarge = baseline.headlineLarge.copy(fontFamily = FontFamily.Serif),
        headlineMedium = baseline.headlineMedium.copy(fontFamily = FontFamily.Serif),
        headlineSmall = baseline.headlineSmall.copy(fontFamily = FontFamily.Serif),
    )
    MaterialTheme(
        typography = typography,
        shapes = Shapes(medium = RoundedCornerShape(14.dp), large = RoundedCornerShape(14.dp)),
        colorScheme = colors,
        content = content,
    )
}
