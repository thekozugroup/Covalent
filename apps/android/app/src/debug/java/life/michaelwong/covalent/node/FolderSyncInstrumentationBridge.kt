package life.michaelwong.covalent.node

import android.content.Context
import android.os.Process
import android.system.Os
import android.system.OsConstants
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.util.UUID

/** Debug-only construction of a second, isolated packaged-engine runtime for device tests. */
internal object FolderSyncInstrumentationBridge {
    fun isolatedPackage(context: Context): PackagedSyncEnginePackage.Verified {
        var invalidStage = "not-started"
        val installed = PackagedSyncEngine.load(context) { invalidStage = it }
            as? PackagedSyncEnginePackage.Verified
            ?: error("The packaged folder-sync engine failed verification at $invalidStage.")
        val privateRoot = context.noBackupFilesDir.canonicalFile
        val rootMetadata = Os.lstat(privateRoot.path)
        check(
            PackagedSyncEngine.privateRuntimeParentAllowed(
                rootMetadata.st_mode,
                rootMetadata.st_uid,
                rootMetadata.st_gid,
                Process.myUid(),
            ),
        )
        repeat(16) {
            val runtime = File(privateRoot, "t" + UUID.randomUUID().toString().take(6)).absoluteFile
            check(runtime.parentFile == privateRoot)
            check(PackagedSyncEngine.runtimeParentPathFits(runtime.path))
            try {
                Os.mkdir(runtime.path, OsConstants.S_IRWXU)
            } catch (error: android.system.ErrnoException) {
                if (error.errno == OsConstants.EEXIST) return@repeat
                throw error
            }
            try {
                val runtimeMetadata = Os.lstat(runtime.path)
                check(
                    PackagedSyncEngine.privateRuntimeChildAllowed(
                        runtimeMetadata.st_mode,
                        runtimeMetadata.st_uid,
                        Process.myUid(),
                    ),
                )
                check(runtime.canonicalFile == runtime)
                return PackagedSyncEnginePackage.Verified(
                    installed.engine.copy(runtimeDirectory = runtime.path),
                )
            } catch (error: Throwable) {
                // Only this newly created, still-empty directory is owned.
                runtime.delete()
                throw error
            }
        }
        error("A unique isolated runtime directory could not be created.")
    }

    /** The socket is closed before native start; a competing bind fails the test safely. */
    fun reserveEphemeralListenerPort(): Int = ServerSocket(
        0,
        1,
        InetAddress.getLoopbackAddress(),
    ).use { socket -> socket.localPort.also { check(it in 1..65_535) } }
}
