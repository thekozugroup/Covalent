package life.michaelwong.covalent.node

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import java.util.concurrent.Executors

/** Reopen only an opted-in identity after unlock/boot or an in-place app update. */
class NodeRestartReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action !in setOf(Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED)) return
        val pending = goAsync()
        val worker = Executors.newSingleThreadExecutor()
        worker.execute {
            try {
                DirectBoot.whenUserUnlocked(context.applicationContext) {
                    runCatching { EmbeddedNodeManager(context.applicationContext).reconnectIfEnabled() }
                        .onFailure { Log.i("CovalentRestart", "Sync restart deferred until the app is opened.") }
                }
            } finally {
                pending.finish()
                worker.shutdown()
            }
        }
    }
}
