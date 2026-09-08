package life.michaelwong.covalent.node

import java.io.File
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderSyncLifecycleContractTest {
    @Test
    fun folderSyncAndBackupDemandRemainIndependent() {
        assertFalse(NodeServiceDemand(false, false).needsService)
        assertTrue(NodeServiceDemand(true, false).needsService)
        assertTrue(NodeServiceDemand(false, true).needsService)
        assertFalse(NodeServiceDemand(false, true).backupIsRunning(true))
        assertTrue(NodeServiceDemand(true, false).backupIsRunning(true))
        assertTrue(
            nodeServiceConfigurationChanged(
                false,
                NodeServiceDemand(true, false),
                true,
                NodeServiceDemand(true, false),
            ),
        )
        assertTrue(
            nodeServiceConfigurationChanged(
                false,
                NodeServiceDemand(true, false),
                false,
                NodeServiceDemand(false, true),
            ),
        )
        assertFalse(
            nodeServiceConfigurationChanged(
                false,
                NodeServiceDemand(false, true),
                false,
                NodeServiceDemand(false, true),
            ),
        )
    }

    @Test
    fun broadStoragePermissionIsDebugOnlyAndNeverRequestedAtStartup() {
        val root = File(System.getProperty("user.dir")).let { start ->
            generateSequence(start) { it.parentFile }.first { File(it, "apps/android/app").isDirectory }
        }
        val mainManifest = File(root, "apps/android/app/src/main/AndroidManifest.xml").readText()
        val debugManifest = File(root, "apps/android/app/src/debug/AndroidManifest.xml").readText()
        val application = File(root, "apps/android/app/src/main/java/life/michaelwong/covalent/CovalentApplication.kt").readText()
        val rawAccess = File(root, "apps/android/app/src/main/java/life/michaelwong/covalent/sync/RawFolderAccess.kt").readText()
        assertFalse(mainManifest.contains("MANAGE_EXTERNAL_STORAGE"))
        assertTrue(debugManifest.contains("MANAGE_EXTERNAL_STORAGE"))
        assertFalse(application.contains("ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION"))
        assertTrue(rawAccess.contains("BuildConfig.DEBUG"))
        assertTrue(rawAccess.contains("O_NOFOLLOW"))
        assertFalse(rawAccess.contains("DocumentsContract"))
    }

    @Test
    fun permissionTransitionStopsTheExactRuntimeBeforeStartingAReplacement() {
        val root = File(System.getProperty("user.dir")).let { start ->
            generateSequence(start) { it.parentFile }.first { File(it, "apps/android/app").isDirectory }
        }
        val service = File(
            root,
            "apps/android/app/src/main/java/life/michaelwong/covalent/node/NodeProviderService.kt",
        ).readText()
        assertTrue(service.contains("HandlerThread(\"covalent-node-owner\")"))
        assertFalse(service.contains("Handler(Looper.getMainLooper())"))
        assertTrue(service.contains("actor.post { handleCommand(action, startId) }"))
        val stop = service.substringAfter("private fun stopProvider()")
            .substringBefore("private fun startNode")
        assertTrue(stop.indexOf("manager.serviceStop(handle)") < stop.indexOf("handle = 0L"))
        assertTrue(stop.contains("if (!response.ok)"))
        assertTrue(stop.indexOf("if (!response.ok)") < stop.indexOf("handle = 0L"))
        val restartBranch = stop.substringAfter("} else if (shouldRestart) {")
            .substringBefore("} else {")
        assertFalse(restartBranch.contains("stopForeground"))
        assertTrue(restartBranch.contains("startAfterReap(command)"))
        val restart = service.substringAfter("private fun beginRestartForAccessChange()")
            .substringBefore("private fun startAfterReap")
        assertTrue(restart.indexOf("reapState.requestStop") < restart.indexOf("stopProvider()"))
    }

    @Test
    fun failedStopsRemainReapingAcrossStartPermissionAndDestroyTransitions() {
        NodeServiceReapState().run {
            requestStop(restartAfterExit = false)
            stopFailed()
            requestStart()
            assertTrue(reaping)
            assertTrue(confirmedExitShouldRestart())
        }
        NodeServiceReapState().run {
            requestStop(restartAfterExit = true)
            stopFailed()
            requestStart() // Permission returned while the exact old runtime was still reaping.
            assertTrue(reaping)
            assertTrue(confirmedExitShouldRestart())
        }
        NodeServiceReapState().run {
            requestStop(restartAfterExit = true)
            stopFailed()
            requestDestroy()
            assertTrue(reaping)
            assertFalse(confirmedExitShouldRestart())
        }
    }

    @Test
    fun processOwnerCannotBeReplacedUntilTheIncumbentReleases() {
        val incumbent = Any()
        val replacement = Any()
        assertTrue(NodeServiceProcessOwner.tryAcquire(incumbent))
        try {
            assertFalse(NodeServiceProcessOwner.tryAcquire(replacement))
            NodeServiceProcessOwner.release(replacement)
            assertFalse(NodeServiceProcessOwner.tryAcquire(replacement))
        } finally {
            NodeServiceProcessOwner.release(incumbent)
        }
        assertTrue(NodeServiceProcessOwner.tryAcquire(replacement))
        NodeServiceProcessOwner.release(replacement)
    }

    @Test
    fun recoveryCannotReplaceOrLoseAnAlreadyOwnedRuntimeHandle() {
        val root = repositoryRoot()
        val service = File(
            root,
            "apps/android/app/src/main/java/life/michaelwong/covalent/node/NodeProviderService.kt",
        ).readText()
        val recovery = service.substringAfter("private fun recover(startId: Int)")
            .substringBefore("private fun startOrRefresh")
        assertTrue(recovery.contains("if (handle > 0L)"))
        assertTrue(recovery.indexOf("if (handle > 0L)") < recovery.indexOf("finishStart("))
        assertTrue(recovery.contains("request?.close()"))
        assertTrue(recovery.contains("scheduleAccessCheck()"))
        assertFalse(recovery.contains("handle = 0L"))
    }

    private fun repositoryRoot(): File = File(System.getProperty("user.dir")).let { start ->
        generateSequence(start) { it.parentFile }.first { File(it, "apps/android/app").isDirectory }
    }
}
