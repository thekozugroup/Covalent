package life.michaelwong.covalent

import android.app.Application
import androidx.work.Configuration
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import life.michaelwong.covalent.node.DirectBoot
import life.michaelwong.covalent.node.EmbeddedNodeManager
import life.michaelwong.covalent.node.NodeHostLifecycle
import life.michaelwong.covalent.appliance.*
import life.michaelwong.covalent.data.CovalentNodeClient

/** Keystore and reconnect work never hold the companion's first frame. */
class InterlinkApplication : Application(), Configuration.Provider, NodeHostLifecycle {
    private val worker = Executors.newSingleThreadExecutor()
    private val mutableManager = MutableStateFlow<EmbeddedNodeManager?>(null)
    private val mutableFailure = MutableStateFlow(false)
    val manager = mutableManager.asStateFlow()
    val failed = mutableFailure.asStateFlow()
    private var importAlerts: ApplianceImportAlertLoop? = null

    @Synchronized override fun nodeHostStarted(owner: Any) {
        val loop = importAlerts ?: run {
            val store = ApplianceSelectionStore(this)
            val api = NodeApplianceApi({ mutableManager.value?.localConnectionForFolderSync() }, CovalentNodeClient(5_000))
            // Status establishes the protected generation; labels/platform are already in the import context.
            val dashboard = ApplianceDashboardController(store, object : ApplianceDashboardApi by api {
                override fun names() = emptyMap<String, String>()
                override fun platform(peerId: String, localId: String): AppliancePlatformObservation? = null
            })
            val imports = ApplianceImportsController(store, api)
            ApplianceImportAlertLoop(read = {
                if (store.read() == null) null else {
                    val state = dashboard.refresh(mutableManager.value?.folderSyncRequested() == true)
                    state.selection?.takeIf { state.connection == ApplianceConnectionState.CONNECTED }?.let {
                        importAlert(imports.refresh(it, true))
                    }
                }
            }, selected = store::read, sink = InterlinkImportNotifications(this)).also { importAlerts = it }
        }
        loop.start(owner)
    }

    @Synchronized override fun nodeHostStopped(owner: Any) { importAlerts?.stop(owner) }

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
