package ar.numis.papin.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

/** Material 3 dark theme (PLAN §7). */
private val DarkColors = darkColorScheme(
    primary = Color(0xFFB69CFF),
    onPrimary = Color(0xFF24005A),
    primaryContainer = Color(0xFF5B3EBD),
    onPrimaryContainer = Color(0xFFE8DEFF),
    secondary = Color(0xFFCBC2DC),
    background = Color(0xFF141218),
    surface = Color(0xFF1B1B1F),
    surfaceVariant = Color(0xFF49454F),
)

@Composable
fun PapinTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = DarkColors,
        content = content,
    )
}
