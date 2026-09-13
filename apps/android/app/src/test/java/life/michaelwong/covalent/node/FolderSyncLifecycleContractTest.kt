package life.michaelwong.covalent.node

import java.io.File
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderSyncLifecycleContractTest {
    @Test
    fun deferredFolderSyncStartsOnlyAfterGrantRegistrationAndCleansFailures() {
        val successfulEvents = mutableListOf<String>()
        assertTrue(
            completeDeferredFolderSyncStart(
                registerPersisted = {
                    successfulEvents += "register"
                    true
                },
                retryFolderSync = { successfulEvents += "retry" },
                cleanup = { successfulEvents += "cleanup" },
            ),
        )
        assertTrue(successfulEvents == listOf("register", "retry"))

        val registrationFailureEvents = mutableListOf<String>()
        assertFalse(
            completeDeferredFolderSyncStart(
                registerPersisted = {
                    registrationFailureEvents += "register"
                    false
                },
                retryFolderSync = { registrationFailureEvents += "retry" },
                cleanup = { registrationFailureEvents += "cleanup" },
            ),
        )
        assertTrue(registrationFailureEvents == listOf("register", "cleanup"))

        val registrationExceptionEvents = mutableListOf<String>()
        assertFalse(
            completeDeferredFolderSyncStart(
                registerPersisted = {
                    registrationExceptionEvents += "register"
                    error("grant registration failed")
                },
                retryFolderSync = { registrationExceptionEvents += "retry" },
                cleanup = { registrationExceptionEvents += "cleanup" },
            ),
        )
        assertTrue(registrationExceptionEvents == listOf("register", "cleanup"))

        val retryFailureEvents = mutableListOf<String>()
        assertFalse(
            completeDeferredFolderSyncStart(
                registerPersisted = {
                    retryFailureEvents += "register"
                    true
                },
                retryFolderSync = {
                    retryFailureEvents += "retry"
                    error("local retry failed")
                },
                cleanup = { retryFailureEvents += "cleanup" },
            ),
        )
        assertTrue(retryFailureEvents == listOf("register", "retry", "cleanup"))
    }

    @Test
    fun unreadableStoresOrPendingChangesBlockWithoutTreatingLostGrantsAsGlobal() {
        var consulted = false
        assertFalse(
            folderSyncAccessUnavailable(false, false) {
                consulted = true
                true
            },
        )
        assertFalse(consulted)
        assertTrue(folderSyncAccessUnavailable(true, false) { false })
        assertTrue(folderSyncAccessUnavailable(true, true) { true })
        // Lost permissions are handled per selected root by the native registry.
        // A readable grant store must not block every unrelated folder link.
        assertFalse(folderSyncAccessUnavailable(true, true) { false })
        assertTrue(folderSyncAccessUnavailable(true, true) { error("unreadable journal") })
    }

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
    fun folderSyncUsesSafWithoutBroadStoragePermission() {
        val root = File(System.getProperty("user.dir")).let { start ->
            generateSequence(start) { it.parentFile }.first { File(it, "apps/android/app").isDirectory }
        }
        val mainManifest = File(root, "apps/android/app/src/main/AndroidManifest.xml").readText()
        val debugManifest = File(root, "apps/android/app/src/debug/AndroidManifest.xml").readText()
        val application = File(root, "apps/android/app/src/main/java/life/michaelwong/covalent/CovalentApplication.kt").readText()
        assertFalse(mainManifest.contains("MANAGE_EXTERNAL_STORAGE"))
        assertFalse(debugManifest.contains("MANAGE_EXTERNAL_STORAGE"))
        assertFalse(application.contains("ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION"))
        val screen = File(root,
            "apps/android/app/src/main/java/life/michaelwong/covalent/ui/FolderSyncScreen.kt").readText()
        assertTrue(screen.contains("rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree())"))
        assertFalse(screen.contains("RawFolderAccess"))
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
        val command = service.substringAfter("ACTION_REFRESH_ACCESS ->")
            .substringBefore("else ->")
        assertTrue(command.contains("if (handle > 0L) beginRestartForAccessChange()"))
        assertTrue(command.contains("else startNode(command.startId)"))
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

    @Test
    fun conditionFeedUsesLanWifiAndMatchesManagedServiceLifetime() {
        val root = repositoryRoot()
        val feed = File(root,
            "apps/android/app/src/main/java/life/michaelwong/covalent/node/AndroidConditionFeed.kt").readText()
        val service = File(root,
            "apps/android/app/src/main/java/life/michaelwong/covalent/node/NodeProviderService.kt").readText()
        assertTrue(feed.contains("TRANSPORT_WIFI"))
        assertFalse(feed.contains("NET_CAPABILITY_VALIDATED"))
        assertFalse(feed.contains("NET_CAPABILITY_INTERNET"))
        assertFalse(feed.contains("activeNetwork"))
        assertTrue(feed.contains("ACTION_BATTERY_CHANGED"))
        assertTrue(feed.contains("registerNetworkCallback(request, callback, actor)"))
        assertTrue(feed.contains("registerReceiver(receiver, filter, null, actor"))
        assertTrue(feed.contains("unregisterNetworkCallback"))
        assertTrue(feed.contains("unregisterReceiver"))
        val started = service.substringAfter("if (response.ok && handle > 0L)")
            .substringBefore("} else {")
        assertTrue(started.indexOf("conditionFeed.start()") < started.indexOf("scheduleAccessCheck()"))
        val stopped = service.substringAfter("private fun stopProvider()")
            .substringBefore("private fun startNode")
        assertTrue(stopped.indexOf("conditionFeed.stop()") < stopped.indexOf("manager.serviceStop(handle)"))
    }

    private fun repositoryRoot(): File = File(System.getProperty("user.dir")).let { start ->
        generateSequence(start) { it.parentFile }.first { File(it, "apps/android/app").isDirectory }
    }
}
