package life.michaelwong.covalent

import android.app.Application
import androidx.work.Configuration
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import life.michaelwong.covalent.node.DirectBoot
import life.michaelwong.covalent.node.EmbeddedNodeManager

/** Keystore and reconnect work never hold the companion's first frame. */
class InterlinkApplication : Application(), Configuration.Provider {
    private val worker = Executors.newSingleThreadExecutor()
    private val mutableManager = MutableStateFlow<EmbeddedNodeManager?>(null)
    private val mutableFailure = MutableStateFlow(false)
    val manager = mutableManager.asStateFlow()
    val failed = mutableFailure.asStateFlow()

    override fun onCreate() {
        super.onCreate()
        DirectBoot.whenUserUnlocked(this) { reconnect() }
    }

    fun reconnect() {
        worker.execute {
            if (!DirectBoot.isUserUnlocked(this)) return@execute
            mutableFailure.value = false
            try {
                val owner = mutableManager.value ?: EmbeddedNodeManager(this)
                mutableManager.value = owner
                owner.reconnectIfEnabled()
            } catch (_: Exception) {
                mutableFailure.value = true
            }
        }
    }

    override val workManagerConfiguration: Configuration = Configuration.Builder()
        .setJobSchedulerJobIdRange(0, 9_999)
        .build()
}
