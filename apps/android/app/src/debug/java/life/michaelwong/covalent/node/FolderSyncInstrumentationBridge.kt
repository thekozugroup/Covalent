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
        val installed = PackagedSyncEngine.load(context) as? PackagedSyncEnginePackage.Verified
            ?: error("The packaged folder-sync engine is unavailable.")
        val privateRoot = context.noBackupFilesDir.canonicalFile
        val rootMetadata = Os.lstat(privateRoot.path)
        check(
            OsConstants.S_ISDIR(rootMetadata.st_mode) &&
                rootMetadata.st_uid == Process.myUid() &&
                rootMetadata.st_mode and (OsConstants.S_IWGRP or OsConstants.S_IWOTH) == 0,
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
                    OsConstants.S_ISDIR(runtimeMetadata.st_mode) &&
                        runtimeMetadata.st_uid == Process.myUid() &&
                        runtimeMetadata.st_mode and 0b111_111_111 == OsConstants.S_IRWXU,
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
