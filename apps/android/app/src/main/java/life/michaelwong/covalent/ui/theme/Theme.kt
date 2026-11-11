package life.michaelwong.covalent.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.Shapes
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

// KozuUI values, hand-translated: the Cobalt accent on Graphite neutrals. KozuUI ships design
// tokens and a web reference but no Compose code, so each Material 3 role is named here against
// the Kozu role it carries. Every role is set on purpose. A role left out falls back to Material's
// baseline purple, which is how tonal buttons, filled cards, dialogs and error text used to drift
// away from the rest of the palette.
private val LightColors = lightColorScheme(
    // Accent
    primary = Color(0xFF2C5AFC), // accent fill
    onPrimary = Color(0xFFFFFFFF), // label on accent
    primaryContainer = Color(0xFFDCEAFF), // accent tint
    onPrimaryContainer = Color(0xFF2844E6), // accent text
    inversePrimary = Color(0xFF96C4FF),
    surfaceTint = Color(0xFF2C5AFC),
    // Neutral supporting roles
    secondary = Color(0xFF5A5C63), // secondary ink
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFEDEEF2), // input tone: tonal buttons, selected chips
    onSecondaryContainer = Color(0xFF0F1012),
    // Success
    tertiary = Color(0xFF00813A),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFF0FDF4),
    onTertiaryContainer = Color(0xFF00813A),
    // Danger
    error = Color(0xFFBF000F),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFFEF2F2),
    onErrorContainer = Color(0xFFBF000F),
    // Canvas, containers and ink
    background = Color(0xFFFCFCFC), // canvas
    onBackground = Color(0xFF0F1012), // ink
    surface = Color(0xFFFFFFFF), // container
    onSurface = Color(0xFF0F1012),
    surfaceVariant = Color(0xFFEDEEF2), // input tone
    onSurfaceVariant = Color(0xFF5A5C63), // secondary ink
    inverseSurface = Color(0xFF0F1012),
    inverseOnSurface = Color(0xFFFCFCFC),
    outline = Color(0xFF8C8F96), // control border, 3:1 on the container
    outlineVariant = Color(0xFFDBDDE1), // hairline
    scrim = Color(0xFF000000),
    surfaceBright = Color(0xFFFFFFFF),
    surfaceDim = Color(0xFFEDEEF2),
    surfaceContainerLowest = Color(0xFFFFFFFF),
    surfaceContainerLow = Color(0xFFFFFFFF),
    surfaceContainer = Color(0xFFF6F7FA), // subtle
    surfaceContainerHigh = Color(0xFFFFFFFF), // dialogs, floating toolbar
    // Filled cards take the subtle tone, not the input tone, so a tonal button stays visible on one.
    surfaceContainerHighest = Color(0xFFF6F7FA),
)

private val DarkColors = darkColorScheme(
    // Accent
    primary = Color(0xFF96C4FF),
    onPrimary = Color(0xFF0F1012),
    primaryContainer = Color(0xFF23378E),
    onPrimaryContainer = Color(0xFF96C4FF),
    inversePrimary = Color(0xFF2C5AFC),
    surfaceTint = Color(0xFF96C4FF),
    // Neutral supporting roles
    secondary = Color(0xFFC0C2C8),
    onSecondary = Color(0xFF0F1012),
    secondaryContainer = Color(0xFF242529),
    onSecondaryContainer = Color(0xFFF5F6F8),
    // Success
    tertiary = Color(0xFF05DF72),
    onTertiary = Color(0xFF0F1012),
    tertiaryContainer = Color(0xFF032E15),
    onTertiaryContainer = Color(0xFF05DF72),
    // Danger
    error = Color(0xFFFFA3A4),
    onError = Color(0xFF0F1012),
    errorContainer = Color(0xFF460809),
    onErrorContainer = Color(0xFFFFA3A4),
    // Canvas, containers and ink
    background = Color(0xFF0A0B0D),
    onBackground = Color(0xFFF5F6F8),
    surface = Color(0xFF151619),
    onSurface = Color(0xFFF5F6F8),
    surfaceVariant = Color(0xFF242529),
    onSurfaceVariant = Color(0xFFC0C2C8),
    inverseSurface = Color(0xFFF5F6F8),
    inverseOnSurface = Color(0xFF0F1012),
    outline = Color(0xFF70737A),
    outlineVariant = Color(0xFF43454B),
    scrim = Color(0xFF000000),
    surfaceBright = Color(0xFF1F2024),
    surfaceDim = Color(0xFF0A0B0D),
    surfaceContainerLowest = Color(0xFF0A0B0D),
    surfaceContainerLow = Color(0xFF151619),
    surfaceContainer = Color(0xFF151619),
    surfaceContainerHigh = Color(0xFF1F2024), // raised: dialogs, floating toolbar
    surfaceContainerHighest = Color(0xFF151619),
)

// One radius set shared with the web console and the Mac app. Buttons stay Material's capsule.
private val CovalentShapes = Shapes(
    extraSmall = RoundedCornerShape(16.dp), // text fields
    small = RoundedCornerShape(12.dp), // chips and blocks nested inside a card
    medium = RoundedCornerShape(28.dp), // cards
    large = RoundedCornerShape(28.dp),
    extraLarge = RoundedCornerShape(28.dp), // dialogs, floating toolbar
)

@Composable
fun CovalentTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    // Off unless a caller asks for it, and no screen does: Covalent keeps one accent family on
    // every surface instead of following the wallpaper.
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

    // Device fonts only: serif headlines, the system sans everywhere else. Sizes and line heights
    // stay at Material's defaults.
    val baseline = Typography()
    val typography = baseline.copy(
        headlineLarge = baseline.headlineLarge.copy(fontFamily = FontFamily.Serif, fontWeight = FontWeight.SemiBold),
        headlineMedium = baseline.headlineMedium.copy(fontFamily = FontFamily.Serif, fontWeight = FontWeight.SemiBold),
        headlineSmall = baseline.headlineSmall.copy(fontFamily = FontFamily.Serif, fontWeight = FontWeight.SemiBold),
        titleLarge = baseline.titleLarge.copy(fontWeight = FontWeight.SemiBold),
        labelLarge = baseline.labelLarge.copy(fontWeight = FontWeight.SemiBold),
    )
    MaterialTheme(
        typography = typography,
        shapes = CovalentShapes,
        colorScheme = colors,
        content = content,
    )
}
