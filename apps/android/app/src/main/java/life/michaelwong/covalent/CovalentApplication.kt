package life.michaelwong.covalent

import android.app.Application
import androidx.work.Configuration
import life.michaelwong.covalent.node.DirectBoot
import life.michaelwong.covalent.node.EmbeddedNodeManager

/** Reserves low JobScheduler IDs for WorkManager; Covalent transfer jobs start at 10,000. */
class CovalentApplication : Application(), Configuration.Provider {
    override fun onCreate() {
        super.onCreate()
        // `onCreate` runs on every process start, including starts that happen before this
        // user has ever unlocked the device — an unattended reboot, a scheduled backup
        // waking the app, an instrumentation run. The embedded node's state is all in
        // credential-encrypted storage, which throws rather than reads in that window, so
        // reading it here crashed the process outright. Wait for the unlock instead; see
        // `DirectBoot` for why none of this moves to device-protected storage.
        DirectBoot.whenUserUnlocked(this) { EmbeddedNodeManager(this).reconnectIfEnabled() }
    }

    override val workManagerConfiguration: Configuration = Configuration.Builder()
        .setJobSchedulerJobIdRange(0, 9_999)
        .build()
}
