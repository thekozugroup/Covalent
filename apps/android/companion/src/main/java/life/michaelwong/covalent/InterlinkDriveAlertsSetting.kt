package life.michaelwong.covalent

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner

@Composable
internal fun InterlinkDriveAlertsSetting() {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val notifications = remember(context) { InterlinkImportNotifications(context.applicationContext) }
    var enabled by remember { mutableStateOf(notifications.enabled()) }
    var settingsFailed by remember { mutableStateOf(false) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { enabled = notifications.enabled() }
    DisposableEffect(lifecycle, notifications) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) enabled = notifications.enabled() }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    fun settings() {
        settingsFailed = runCatching {
            context.startActivity(Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                .putExtra(Settings.EXTRA_CHANNEL_ID, InterlinkImportNotifications.CHANNEL))
        }.isFailure
    }
    Card(Modifier.fillMaxWidth().testTag("interlink-alert-settings")) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(if (enabled) R.string.interlink_alert_enabled else R.string.interlink_alert_disabled),
                style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.interlink_alert_detail))
            if (!enabled && Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                Button(onClick = { permission.launch(Manifest.permission.POST_NOTIFICATIONS) }) {
                    Text(stringResource(R.string.interlink_alert_allow))
                }
            }
            TextButton(onClick = ::settings) { Text(stringResource(R.string.interlink_alert_settings)) }
            if (settingsFailed) Text(stringResource(R.string.interlink_alert_settings_failed), color = MaterialTheme.colorScheme.error)
        }
    }
}
