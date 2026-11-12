package eu.vardalabs.organizer.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val Green = Color(0xFF2E5E4E)

/** Material 3: the wallpaper's colours where Android has them (12+), else Organizer's green. */
@Composable
fun OrganizerTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val colors =
        when {
            android.os.Build.VERSION.SDK_INT >= 31 && dark -> dynamicDarkColorScheme(context)
            android.os.Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(context)
            dark -> darkColorScheme(primary = Color(0xFF9BD1BC))
            else -> lightColorScheme(primary = Green)
        }
    MaterialTheme(colorScheme = colors, content = content)
}
