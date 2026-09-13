package life.michaelwong.covalent.node

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Rect
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.system.ErrnoException
import android.system.Os
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.ComposeTimeoutException
import androidx.compose.ui.test.assertContentDescriptionEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isDialog
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.isToggleable
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performTextInput
import androidx.documentfile.provider.DocumentFile
import androidx.test.core.app.ApplicationProvider
import androidx.test.espresso.Espresso.closeSoftKeyboard
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.Inet6Address
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.ServerSocket
import java.nio.charset.StandardCharsets
import java.nio.file.DirectoryStream
import java.nio.file.Files
import java.nio.file.Path
import java.security.SecureRandom
import java.util.Base64
import java.util.UUID
import java.util.concurrent.TimeUnit
import life.michaelwong.covalent.BuildConfig
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.model.AndroidLinkConditions
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderLinkCadence
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkRunPhase
import life.michaelwong.covalent.model.FolderLinkRunResult
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderLinkSettingsState
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.NetworkPairingDirection
import life.michaelwong.covalent.model.NetworkPairingState
import life.michaelwong.covalent.model.NodeConnection
import life.michaelwong.covalent.sync.FolderSyncGrantStore
import life.michaelwong.covalent.sync.SafFolderGrant
import life.michaelwong.covalent.sync.SafFolderGrantStore
import life.michaelwong.covalent.sync.SafWebDavServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import life.michaelwong.covalent.ui.FolderSyncScreen
import life.michaelwong.covalent.ui.theme.CovalentTheme
import org.junit.Rule
import org.junit.Test

/**
 * Self-contained API-37 proof of the Android UI, system folder grants, JNI, and packaged workers.
 *
 * The test owns one exact ExternalStorageProvider fixture and removes it on every exit path.
 */
class SafFolderSyncJourneyInstrumentedTest {
    @get:Rule
    val compose = createComposeRule()
    private val showScreen = mutableStateOf(true)
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val client = CovalentNodeClient()
    private var secondHandle = 0L
    private var secondApiToken: String? = null
    private var secondKeyEncryptionKey: ByteArray? = null
    private var secondFolderSyncListenerPort: Int? = null
    private var offerId: String? = null
    private var installedEngine: VerifiedPackagedSyncEngine? = null
    private var secondData: File? = null
    private val secondSafServers = mutableListOf<SafWebDavServer>()

    @Test
    fun nativeManualSafRunRegistersAcrossRestartAndIsolatesLostGrant() {
        assertTrue("This folder-picker fixture runs only on an emulator", shell("getprop ro.kernel.qemu") == "1")
        assertTrue(BuildConfig.DEBUG)
        assertTrue(BuildConfig.COVALENT_SYNC_ENGINE_PACKAGED)
        assertTrue(CovalentNative.isAvailable)
        val runId = UUID.randomUUID().toString().replace("-", "")
        val manager = EmbeddedNodeManager(context)
        val selectedUris = mutableListOf<Uri>()
        val pickerFixture = "$SAF_FIXTURE_PREFIX$runId"
        assertEquals(
            PackageManager.PERMISSION_GRANTED,
            context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK),
        )
        try {
            prepareSystemPickerFixture(pickerFixture)
            val packageValue = FolderSyncInstrumentationBridge.isolatedPackage(context)
            installedEngine = packageValue.engine
            assertTrue(manager.enableFolderSyncHost())
            awaitValue("initial deferred service API") { manager.liveConnection() }
            compose.setContent { CovalentTheme { if (showScreen.value) FolderSyncScreen(manager) } }
            val destinationGrant = chooseSystemFolder("destination", pickerFixture, "destination", selectedUris)
            val sourceGrant = chooseSystemFolder("source", pickerFixture, "source", selectedUris)
            writeSafFile(sourceGrant, "manual.txt", SAF_MANUAL_CONTENT)
            writeSafFile(sourceGrant, SAF_UNICODE_NAME, SAF_UNICODE_CONTENT)
            val connectionA = awaitValue("service API after persisted grants") { manager.liveConnection() }

            var connectionB = startSecondNode(packageValue, runId = runId)
            registerSecondNodeGrant(connectionB, destinationGrant)
            val identityA = client.transportIdentity(connectionA.baseUrl, connectionA.token)
            val identityB = client.transportIdentity(connectionB.baseUrl, connectionB.token)
            assertTrue(identityA.deviceId != identityB.deviceId)
            pairUsingPhoneUi(connectionA, connectionB, advertisedGuestPeerAddress(identityB.peerPort))
            closeSoftKeyboard()

            val settings = FolderLinkSettings(FolderLinkPolicy(), false, FolderLinkCadence.Manual)
            val labelMatcher = hasSetTextAction() and hasText(context.getString(R.string.folder_sync_label))
            compose.onNodeWithTag("folder-sync-list").performScrollToNode(labelMatcher)
            compose.onNode(labelMatcher).performTextInput(SAF_FOLDER_LABEL)
            closeSoftKeyboard()
            clickScreenText("API 37 isolated peer")
            visibleScreenText(context.getString(R.string.folder_link_direction)).assertIsDisplayed()
            visibleScreenText(
                context.getString(R.string.folder_link_source_folder, "source"),
            ).assertIsDisplayed()
            visibleScreenText(
                context.getString(R.string.folder_link_destination_peer, "API 37 isolated peer"),
            ).assertIsDisplayed()
            visibleScreenText(context.getString(R.string.folder_link_source_deletions_detail)).assertIsDisplayed()
            visibleScreenText(context.getString(R.string.folder_link_destination_deletions_detail)).assertIsDisplayed()
            clickScreenText(context.getString(R.string.folder_link_cadence_manual))
            clickScreenText(context.getString(R.string.folder_sync_offer))
            val createdShare = awaitValue("native SAF manual offer recorded") {
                client.folderSyncStatus(connectionA.baseUrl, connectionA.token).shares
                    .singleOrNull {
                        !it.incoming && it.peerId == identityB.deviceId && it.label == SAF_FOLDER_LABEL
                    }
            }
            val folderId = UUID.fromString(createdShare.folderId)
            offerId = createdShare.offerId
            await("incoming SAF manual offer") {
                client.folderSyncStatus(connectionB.baseUrl, connectionB.token).shares.any {
                    it.offerId == offerId && it.incoming && it.phase == FolderSharePhase.OFFERED
                }
            }
            client.acceptFolder(
                connectionB.baseUrl,
                connectionB.token,
                checkNotNull(offerId),
                destinationGrant.selectedRoot,
            )
            val settingsState = awaitSharedSettings(connectionA, connectionB, settings)
            assertSafFileAbsentFor(destinationGrant, "manual.txt", NEGATIVE_WINDOW_MILLIS)
            clickScreenText(context.getString(R.string.folder_link_run_now))
            retrySavedRunIfVisible()
            try {
                awaitSafFile(
                    "public Manual Run Now through SAF",
                    destinationGrant,
                    "manual.txt",
                    SAF_MANUAL_CONTENT,
                )
            } catch (failure: AssertionError) {
                throw AssertionError(
                    "${failure.message}\n" +
                        recoveryDiagnostic("source", connectionA, "initial-manual") +
                        "\n" +
                        recoveryDiagnostic("destination", connectionB, "initial-manual"),
                    failure,
                )
            }
            awaitSafFile(
                "Unicode filename through SAF",
                destinationGrant,
                SAF_UNICODE_NAME,
                SAF_UNICODE_CONTENT,
            )
            val firstRun = awaitValue("first SAF run completion") {
                sourceShare(connectionA).linkRun?.takeIf { it.phase == FolderLinkRunPhase.SUCCEEDED }
            }
            assertEquals(1L, firstRun.generation)
            awaitExactHelperCounts(0, 0)
            clickScreenText(context.getString(R.string.folder_link_settings_edit))
            clickDialogText(context.getString(R.string.action_cancel))

            val oldWorkerPort = checkNotNull(secondFolderSyncListenerPort)
            val movedPeerPort = reserveEphemeralPeerPort(identityB.peerPort)
            val movedWorkerPort = reserveEphemeralWorkerPort(oldWorkerPort)
            assertTrue(CovalentNative.stop(secondHandle).ok)
            secondHandle = 0
            secondSafServers.forEach { it.close() }
            secondSafServers.clear()
            awaitExactHelperCounts(0, 0)
            await("old second-node listeners released") {
                canBindPeerPort(identityB.peerPort) && canBindWorkerPort(oldWorkerPort)
            }
            connectionB = startSecondNode(
                packageValue,
                peerListenerPort = movedPeerPort,
                folderSyncListenerPort = movedWorkerPort,
                runId = runId,
            )
            registerSecondNodeGrant(connectionB, destinationGrant)
            val movedIdentity = client.transportIdentity(connectionB.baseUrl, connectionB.token)
            assertEquals(identityB.deviceId, movedIdentity.deviceId)
            assertEquals(identityB.certificateFingerprint, movedIdentity.certificateFingerprint)
            assertEquals(identityB.certificateDer, movedIdentity.certificateDer)
            assertEquals(movedPeerPort, movedIdentity.peerPort)
            assertTrue(identityB.peerPort != movedIdentity.peerPort)
            assertEquals(movedWorkerPort, secondFolderSyncListenerPort)
            assertTrue(oldWorkerPort != movedWorkerPort)

            val movedPeerAddress = advertisedGuestPeerAddress(movedPeerPort)
            val addressButtonTag = "folder-peer-address-${identityB.deviceId}"
            compose.onNodeWithTag("folder-sync-list")
                .performScrollToNode(hasTestTag(addressButtonTag))
            compose.onNodeWithTag(addressButtonTag).performClick()
            compose.onNodeWithTag("folder-peer-address-input").performTextInput(movedPeerAddress)
            compose.onNodeWithTag("folder-peer-address-confirm").performClick()
            await("native address refresh becomes authoritative") {
                client.folderSyncStatus(connectionA.baseUrl, connectionA.token).peers
                    .singleOrNull { it.peerId == identityB.deviceId }
                    ?.address == movedPeerAddress
            }
            writeSafFile(sourceGrant, "after-address.txt", SAF_ADDRESS_CONTENT)
            val addressShare = sourceShare(connectionA)
            client.runFolderLinkNow(
                connectionA.baseUrl,
                connectionA.token,
                folderId.toString(),
                UUID.randomUUID().toString(),
                expectedGeneration = checkNotNull(addressShare.linkRun).generation,
                settingsRevision = checkNotNull(addressShare.linkSettings).revision,
            )
            awaitSafFile(
                "Manual Run Now after native address refresh",
                destinationGrant,
                "after-address.txt",
                SAF_ADDRESS_CONTENT,
            )
            val addressRun = awaitValue("address-refreshed SAF run completion") {
                sourceShare(connectionA).linkRun?.takeIf {
                    it.generation == firstRun.generation + 1 && it.phase == FolderLinkRunPhase.SUCCEEDED
                }
            }
            awaitExactHelperCounts(0, 0)

            val firstApi = connectionA.baseUrl
            manager.refreshFolderSyncAccess()
            val restartedA = awaitValue("service API after SAF restart registration") {
                manager.liveConnection()?.takeIf { it.baseUrl != firstApi }
            }
            assertEquals(
                movedPeerAddress,
                client.folderSyncStatus(restartedA.baseUrl, restartedA.token).peers
                    .single { it.peerId == identityB.deviceId }.address,
            )
            writeSafFile(sourceGrant, "restart.txt", SAF_RESTART_CONTENT)
            val restartedShare = sourceShare(restartedA)
            client.runFolderLinkNow(
                restartedA.baseUrl,
                restartedA.token,
                folderId.toString(),
                UUID.randomUUID().toString(),
                expectedGeneration = checkNotNull(restartedShare.linkRun).generation,
                settingsRevision = checkNotNull(restartedShare.linkSettings).revision,
            )
            awaitSafFile(
                "Manual Run Now after SAF grant re-registration",
                destinationGrant,
                "restart.txt",
                SAF_RESTART_CONTENT,
            )
            awaitValue("restarted SAF run completion") {
                sourceShare(restartedA).linkRun?.takeIf {
                    it.generation == addressRun.generation + 1 && it.phase == FolderLinkRunPhase.SUCCEEDED
                }
            }
            awaitExactHelperCounts(0, 0)
            writeSafFile(sourceGrant, "recovery.txt", SAF_RECOVERY_CONTENT)

            context.contentResolver.releasePersistableUriPermission(
                sourceGrant.treeUri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
            selectedUris.remove(sourceGrant.treeUri)
            manager.refreshFolderSyncAccess()
            val lostGrantConnection = awaitValue("service API after active SAF grant loss") {
                manager.liveConnection()?.takeIf { it.baseUrl != restartedA.baseUrl }
            }
            val beforeLostGrantRun = sourceShare(lostGrantConnection)
            client.runFolderLinkNow(
                lostGrantConnection.baseUrl,
                lostGrantConnection.token,
                folderId.toString(),
                UUID.randomUUID().toString(),
                expectedGeneration = checkNotNull(beforeLostGrantRun.linkRun).generation,
                settingsRevision = checkNotNull(beforeLostGrantRun.linkSettings).revision,
            )
            val lostStatus = awaitValue("active lost grant isolated to its folder") {
                client.folderSyncStatus(lostGrantConnection.baseUrl, lostGrantConnection.token).takeIf { status ->
                    status.availability == FolderSyncAvailability.AVAILABLE &&
                        status.issue == null &&
                        status.folders.singleOrNull { it.folderId == folderId.toString() }
                            ?.let { it.state == "error" && it.accessUnavailable } == true &&
                        sourceShare(lostGrantConnection).linkRun?.phase == FolderLinkRunPhase.INTERRUPTED
                }
            }
            assertTrue(lostStatus.shares.any { it.folderId == folderId.toString() })
            val interruptedRun = checkNotNull(sourceShare(lostGrantConnection).linkRun)
            assertEquals(FolderLinkRunPhase.INTERRUPTED, interruptedRun.phase)
            assertTrue(interruptedRun.destinations.all { it.result == FolderLinkRunResult.INTERRUPTED })
            val lostFolderState = lostStatus.folders
                .singleOrNull { it.folderId == folderId.toString() }
                ?.state ?: "idle-without-worker"
            println(
                "COVALENT_ACTIVE_LOST_GRANT availability=${lostStatus.availability} " +
                    "lifecycle=${lostStatus.lifecycle} issue=${lostStatus.issue} " +
                    "folderState=$lostFolderState runPhase=${sourceShare(lostGrantConnection).linkRun?.phase}",
            )
            awaitExactHelperCounts(0, 0)

            assertAnyVisibleScreenText(context.getString(R.string.folder_sync_choose_again_detail))
            val restoredGrant = rechooseSystemFolder(
                "source-recovery",
                pickerFixture,
                "source",
                sourceGrant,
                selectedUris,
            )
            assertEquals(sourceGrant.id, restoredGrant.id)
            clickScreenText(context.getString(R.string.folder_sync_use_selected_again))
            val repairedA = try {
                awaitStableFolderConnection(
                    "active SAF grant repaired for its exact folder",
                    manager,
                    lostGrantConnection.baseUrl,
                    sourceGrant,
                    checkNotNull(offerId),
                ) { status ->
                    val restoredRecord = SafFolderGrantStore(context).records().singleOrNull {
                        it.id == sourceGrant.id && it.treeUri == sourceGrant.treeUri
                    }
                    restoredRecord?.selectedRoot == sourceGrant.selectedRoot &&
                        status.shares.singleOrNull { it.offerId == offerId }
                            ?.let { it.folderId == folderId.toString() && it.phase == FolderSharePhase.READY } == true &&
                        status.folders.none {
                            it.folderId == folderId.toString() && it.accessUnavailable
                        }
                }
            } catch (failure: AssertionError) {
                val retryText = context.getString(R.string.folder_sync_retry_saved_repair)
                val retryCandidates = compose.onAllNodesWithText(retryText)
                val retryNodes = runCatching { retryCandidates.fetchSemanticsNodes() }.getOrDefault(emptyList())
                val retryVisible = retryNodes.indices.any { index ->
                    runCatching {
                        retryCandidates[index].assertIsDisplayed()
                        true
                    }.getOrDefault(false)
                }
                throw AssertionError(
                    "${failure.message} retrySavedRepairNodeCount=${retryNodes.size} " +
                        "retrySavedRepairVisible=$retryVisible",
                    failure,
                )
            }
            val repairedShare = sourceShare(repairedA)
            val repairedGeneration = checkNotNull(repairedShare.linkRun).generation
            val recoveryRequestId = UUID.randomUUID().toString()
            val recoveryMutation = client.runFolderLinkNow(
                repairedA.baseUrl,
                repairedA.token,
                folderId.toString(),
                recoveryRequestId,
                expectedGeneration = repairedGeneration,
                settingsRevision = checkNotNull(repairedShare.linkSettings).revision,
            )
            println(
                    "COVALENT_RECOVERY_ACK priorBaseUrl=${lostGrantConnection.baseUrl} " +
                    "repairedBaseUrl=${repairedA.baseUrl} offerId=${recoveryMutation.offerId} " +
                    "requestId=$recoveryRequestId expectedGeneration=$repairedGeneration",
            )
            try {
                awaitSafFile(
                    "Manual Run Now after exact SAF grant repair",
                    destinationGrant,
                    "recovery.txt",
                    SAF_RECOVERY_CONTENT,
                )
            } catch (failure: AssertionError) {
                println(recoveryDiagnostic("source", repairedA, recoveryRequestId))
                println(recoveryDiagnostic("destination", connectionB, recoveryRequestId))
                val retryText = context.getString(R.string.folder_link_run_retry_exact)
                val visibleRetry = compose.onAllNodesWithText(retryText).fetchSemanticsNodes().indices.any { index ->
                    runCatching {
                        compose.onAllNodesWithText(retryText)[index].assertIsDisplayed()
                        true
                    }.getOrDefault(false)
                }
                println("COVALENT_RECOVERY_UI visibleExactRunRetry=$visibleRetry")
                throw failure
            }
            val repairedRun = awaitValue("repaired SAF run completion") {
                sourceShare(repairedA).linkRun?.takeIf { it.phase == FolderLinkRunPhase.SUCCEEDED }
            }
            awaitExactHelperCounts(0, 0)

            writeSafFile(destinationGrant, "destination-only.txt", SAF_REVERSE_CONTENT)
            assertSafFileAbsentFor(sourceGrant, "destination-only.txt", NEGATIVE_WINDOW_MILLIS)

            clickScreenText(context.getString(R.string.folder_link_settings_edit))
            clickDialogText(context.getString(R.string.folder_link_cadence_continuous))
            clickToggle("folder-link-settings-source-deletions")
            clickToggle("folder-link-settings-destination-deletions")
            clickDialogText(context.getString(R.string.folder_link_settings_save))
            assertDialogText(context.getString(R.string.folder_link_settings_confirm_source_deletions))
            assertDialogText(context.getString(R.string.folder_link_settings_confirm_restore_deletions))
            clickDialogText(context.getString(R.string.folder_link_settings_confirm))
            retrySavedSettingsIfVisible()
            val continuousSettings = settings.copy(
                cadence = FolderLinkCadence.Continuous,
                deletionPolicy = FolderLinkPolicy(
                    propagateSourceDeletions = true,
                    restoreLocalDeletions = true,
                ),
            )
            val continuousState = awaitSharedSettings(
                repairedA,
                connectionB,
                continuousSettings,
                settingsState.revision + 1,
            )
            assertAnyVisibleScreenText(context.getString(R.string.folder_link_source_deletions_propagate))
            assertAnyVisibleScreenText(context.getString(R.string.folder_link_destination_deletions_restore))
            awaitExactHelperCounts(1, 1)

            writeSafFile(sourceGrant, "continuous.txt", SAF_CONTINUOUS_CONTENT)
            awaitSafFile(
                "Continuous SAF transfer",
                destinationGrant,
                "continuous.txt",
                SAF_CONTINUOUS_CONTENT,
            )
            clickScreenText(context.getString(R.string.folder_link_pause))
            awaitSharedSettings(
                repairedA,
                connectionB,
                continuousSettings.copy(paused = true),
                continuousState.revision + 1,
            )
            awaitExactHelperCounts(0, 0)
            writeSafFile(sourceGrant, "paused.txt", SAF_PAUSED_CONTENT)
            assertSafFileAbsentFor(destinationGrant, "paused.txt", NEGATIVE_WINDOW_MILLIS)
            clickScreenText(context.getString(R.string.folder_link_resume))
            awaitSharedSettings(
                repairedA,
                connectionB,
                continuousSettings,
                continuousState.revision + 2,
            )
            awaitSafFile(
                "resumed Continuous SAF transfer",
                destinationGrant,
                "paused.txt",
                SAF_PAUSED_CONTENT,
            )
            awaitExactHelperCounts(1, 1)

            writeSafFile(sourceGrant, "restore-destination.txt", SAF_RESTORE_CONTENT)
            awaitSafFile(
                "destination restoration seed",
                destinationGrant,
                "restore-destination.txt",
                SAF_RESTORE_CONTENT,
            )
            assertTrue(deleteSafFile(destinationGrant, "restore-destination.txt"))
            awaitSafFile(
                "destination deletion restored by shared settings",
                destinationGrant,
                "restore-destination.txt",
                SAF_RESTORE_CONTENT,
            )

            writeSafFile(sourceGrant, "propagate-source.txt", SAF_PROPAGATE_CONTENT)
            awaitSafFile(
                "source deletion seed",
                destinationGrant,
                "propagate-source.txt",
                SAF_PROPAGATE_CONTENT,
            )
            val seedVisibleAtUnixMs = System.currentTimeMillis()
            assertTrue(deleteSafFile(sourceGrant, "propagate-source.txt"))
            assertTrue(readSafFile(sourceGrant, "propagate-source.txt") == null)
            val runAfterDeletion = sourceShare(repairedA).linkRun
            println(
                "COVALENT_SOURCE_DELETION_STAGE stage=source-deleted " +
                    "seedVisibleAtUnixMs=$seedVisibleAtUnixMs " +
                    "sourceAbsentAtUnixMs=${System.currentTimeMillis()} " +
                    "observedAfterDeletionGeneration=${runAfterDeletion?.generation} " +
                    "observedAfterDeletionPhase=${runAfterDeletion?.phase}",
            )
            try {
                await("source deletion propagated by shared settings", TRANSFER_TIMEOUT_MILLIS) {
                    readSafFile(destinationGrant, "propagate-source.txt") == null
                }
            } catch (failure: AssertionError) {
                throw AssertionError(
                    "${failure.message}\n" +
                        recoveryDiagnostic("source", repairedA, "source-deletion") +
                        "\n" +
                        recoveryDiagnostic("destination", connectionB, "source-deletion"),
                    failure,
                )
            }

            clickScreenText(context.getString(R.string.folder_sync_remove))
            clickDialogText(context.getString(R.string.folder_sync_keep_sharing))
            assertTrue(sourceShare(repairedA).phase != FolderSharePhase.REMOVED)
            clickScreenText(context.getString(R.string.folder_sync_remove))
            compose.onNode(
                hasText(context.getString(R.string.folder_sync_remove)) and hasAnyAncestor(isDialog()),
            ).performClick()
            await("native SAF share removal recorded") {
                sourceShare(repairedA).phase == FolderSharePhase.REMOVED &&
                    client.folderSyncStatus(connectionB.baseUrl, connectionB.token).shares
                        .single { it.offerId == offerId }.phase == FolderSharePhase.REMOVED
            }
            awaitExactHelperCounts(0, 0)
        } finally {
            instrumentation.runOnMainSync { showScreen.value = false }
            secondSafServers.forEach { runCatching { it.close() } }
            secondSafServers.clear()
            if (secondHandle > 0) {
                runCatching { CovalentNative.stop(secondHandle) }
                secondHandle = 0
            }
            forgetSecondNodeSecrets()
            context.getSharedPreferences("covalent_embedded_provider", Context.MODE_PRIVATE).edit()
                .putBoolean("folder_sync_requested", false)
                .putBoolean("enabled", false)
                .putBoolean("running", false)
                .commit()
            context.stopService(Intent(context, NodeProviderService::class.java))
            val stoppedHelpers = runCatching {
                if (installedEngine != null) awaitExactHelperCounts(0, 0)
            }
            context.getSharedPreferences("covalent_saf_folder_grants", Context.MODE_PRIVATE)
                .edit().clear().commit()
            selectedUris.forEach { uri ->
                runCatching {
                    context.contentResolver.releasePersistableUriPermission(
                        uri,
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
                    )
                }
            }
            secondData?.takeIf(File::exists)?.let(::deleteTreeWithoutFollowingLinks)
            removeSystemPickerFixture(pickerFixture)
            stoppedHelpers.getOrThrow()
        }
        assertFalse(secondDataDirectory(runId).exists())
    }

    private fun visibleScreenText(
        value: String,
        failureContext: (() -> String)? = null,
    ): SemanticsNodeInteraction {
        val list = compose.onNodeWithTag("folder-sync-list")
        val target = compose.onNodeWithText(value)
        var lastFailure: Throwable? = null
        try {
            compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
                runCatching {
                    // A lazy item can be taller than the viewport. First compose its matching item,
                    // then ask the scrollable ancestor to expose the exact descendant within it.
                    list.performScrollToNode(hasText(value))
                    target.performScrollTo()
                    target.assertIsDisplayed()
                    true
                }.onFailure { lastFailure = it }.getOrDefault(false)
            }
        } catch (timeout: ComposeTimeoutException) {
            val nodes = runCatching {
                compose.onAllNodesWithText(value).fetchSemanticsNodes()
            }.getOrDefault(emptyList())
            val bounds = nodes.take(MAX_DIAGNOSTIC_NODES).joinToString(prefix = "[", postfix = "]") {
                val rect = it.boundsInRoot
                "(${rect.left.toInt()},${rect.top.toInt()},${rect.right.toInt()},${rect.bottom.toInt()})"
            }
            val context = failureContext?.let { diagnostic ->
                runCatching { diagnostic() }.getOrElse { error ->
                    "diagnosticError=${error.javaClass.name}"
                }
            } ?: ""
            throw AssertionError(
                "Screen text did not become visible: expectedNodeCount=1 " +
                    "actualNodeCount=${nodes.size} boundsInRoot=$bounds " +
                    "lastError=${lastFailure?.javaClass?.name ?: "none"} $context",
                timeout,
            )
        }
        return target
    }

    private fun clickScreenText(value: String) {
        val target = visibleScreenText(value)
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { target.assertIsEnabled(); true }.getOrDefault(false)
        }
        target.performClick()
    }

    private fun assertDialogText(value: String): SemanticsNodeInteraction {
        val target = compose.onNode(hasText(value) and hasAnyAncestor(isDialog()))
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { target.assertIsDisplayed(); true }.getOrDefault(false) ||
                runCatching {
                    target.performScrollTo()
                    target.assertIsDisplayed()
                    true
                }.getOrDefault(false)
        }
        return target
    }

    private fun clickDialogText(value: String) {
        val target = assertDialogText(value)
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { target.assertIsEnabled(); true }.getOrDefault(false)
        }
        target.performClick()
    }

    private fun assertAnyVisibleScreenText(value: String) {
        val list = compose.onNodeWithTag("folder-sync-list")
        val candidates = compose.onAllNodesWithText(value)
        var lastFailure: Throwable? = null
        try {
            compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
                runCatching { list.performScrollToNode(hasText(value)) }
                    .onFailure { lastFailure = it }
                candidates.fetchSemanticsNodes().indices.any { index ->
                    runCatching {
                        candidates[index].performScrollTo()
                        candidates[index].assertIsDisplayed()
                        true
                    }.onFailure { lastFailure = it }.getOrDefault(false)
                }
            }
        } catch (timeout: ComposeTimeoutException) {
            val nodes = runCatching { candidates.fetchSemanticsNodes() }.getOrDefault(emptyList())
            val bounds = nodes.take(MAX_DIAGNOSTIC_NODES).joinToString(prefix = "[", postfix = "]") {
                val rect = it.boundsInRoot
                "(${rect.left.toInt()},${rect.top.toInt()},${rect.right.toInt()},${rect.bottom.toInt()})"
            }
            throw AssertionError(
                "Matching screen text did not become visible: expected=[$value] " +
                    "actualNodeCount=${nodes.size} " +
                    "boundsInRoot=$bounds lastError=${lastFailure?.javaClass?.name ?: "none"}",
                timeout,
            )
        }
    }

    private fun clickToggle(cardTag: String) {
        val target = compose.onNode(
            isToggleable() and hasAnyAncestor(hasTestTag(cardTag)),
            useUnmergedTree = true,
        )
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { target.assertIsDisplayed().assertIsEnabled(); true }.getOrDefault(false)
        }
        target.performClick()
    }

    private fun retrySavedSettingsIfVisible() {
        retrySavedRequestIfVisible(context.getString(R.string.folder_link_settings_retry_exact))
    }

    private fun retrySavedRunIfVisible() {
        retrySavedRequestIfVisible(context.getString(R.string.folder_link_run_retry_exact))
    }

    private fun retrySavedRequestIfVisible(text: String) {
        repeat(20) {
            compose.waitForIdle()
            val candidates = compose.onAllNodesWithText(text)
            if (candidates.fetchSemanticsNodes().indices.any { index ->
                    runCatching {
                        candidates[index].performScrollTo()
                        candidates[index].assertIsDisplayed().assertIsEnabled()
                        candidates[index].performClick()
                        true
                    }.getOrDefault(false)
                }
            ) {
                return
            }
            Thread.sleep(250)
        }
    }

    private fun sourceShare(connection: NodeConnection): FolderShare =
        client.folderSyncStatus(connection.baseUrl, connection.token).shares.single {
            !it.incoming && it.offerId == offerId
        }

    private fun recoveryDiagnostic(label: String, connection: NodeConnection, requestId: String): String =
        runCatching {
            val status = client.folderSyncStatus(connection.baseUrl, connection.token)
            val share = status.shares.singleOrNull { it.offerId == offerId }
            val run = share?.linkRun
            val folder = share?.let { current ->
                status.folders.singleOrNull { it.folderId == current.folderId }
            }
            "COVALENT_RECOVERY_DIAGNOSTIC node=$label requestId=$requestId " +
                "availability=${status.availability} lifecycle=${status.lifecycle} issue=${status.issue} " +
                "sharePhase=${share?.phase} peer=${share?.peerConnection} " +
                "settingsRevision=${share?.linkSettings?.revision} settingsConfirmed=${share?.linkSettings?.confirmed} " +
                "runGeneration=${run?.generation} runPhase=${run?.phase} " +
                "pendingRequestId=${run?.pendingRequest?.requestId} " +
                "rejectedRequestId=${run?.rejectedRequest?.requestId} " +
                "destinationResults=${run?.destinations?.map { it.result }} " +
                "folderState=${folder?.state} accessUnavailable=${folder?.accessUnavailable} " +
                "scanPullErrors=${folder?.scanPullErrorCount} statusError=${folder?.statusError}"
        }.getOrElse { error ->
            "COVALENT_RECOVERY_DIAGNOSTIC node=$label requestId=$requestId error=${error.javaClass.name}"
        }

    private fun awaitStableFolderConnection(
        label: String,
        manager: EmbeddedNodeManager,
        excludedBaseUrl: String,
        grant: SafFolderGrant,
        repairOfferId: String,
        accepts: (life.michaelwong.covalent.model.FolderSyncStatus) -> Boolean,
    ): NodeConnection {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(DEFAULT_TIMEOUT_MILLIS)
        var candidate: NodeConnection? = null
        var stableSince = 0L
        var sawChangedBaseUrl = false
        var lastLifecycle = "none"
        var lastAvailability = "none"
        var lastIssue = "none"
        var lastSharePhase = "none"
        var lastFolderState = "none"
        var lastAccessUnavailable = "none"
        var lastErrorClass = "none"
        var lastHttpStatus = "none"
        while (System.nanoTime() < deadline) {
            val current = manager.liveConnection()
            val changedConnection = current?.takeIf { it.baseUrl != excludedBaseUrl }
            sawChangedBaseUrl = sawChangedBaseUrl || changedConnection != null
            val accepted = changedConnection?.let { connection ->
                try {
                    val status = client.folderSyncStatus(connection.baseUrl, connection.token)
                    val share = status.shares.singleOrNull { it.offerId == repairOfferId }
                    val folder = share?.let { currentShare ->
                        status.folders.singleOrNull { it.folderId == currentShare.folderId }
                    }
                    lastLifecycle = status.lifecycle.toString()
                    lastAvailability = status.availability.toString()
                    lastIssue = status.issue?.toString() ?: "none"
                    lastSharePhase = share?.phase?.toString() ?: "none"
                    lastFolderState = folder?.state ?: "none"
                    lastAccessUnavailable = folder?.accessUnavailable?.toString() ?: "none"
                    accepts(status)
                } catch (error: Throwable) {
                    lastErrorClass = error.javaClass.name
                    lastHttpStatus = (error as? NodeApiException)?.statusCode?.toString() ?: "none"
                    false
                }
            } == true
            if (!accepted) {
                candidate = null
                stableSince = 0L
            } else if (candidate?.baseUrl != current?.baseUrl) {
                candidate = current
                stableSince = System.nanoTime()
            } else if (System.nanoTime() - stableSince >= TimeUnit.MILLISECONDS.toNanos(STABLE_API_MILLIS)) {
                return checkNotNull(current)
            }
            Thread.sleep(POLL_MILLIS)
        }
        val persistedPermission = runCatching {
            context.contentResolver.persistedUriPermissions.any { permission ->
                permission.uri == grant.treeUri && permission.isReadPermission && permission.isWritePermission
            }
        }.getOrNull()
        val pendingRepair = runCatching {
            FolderSyncGrantStore(context).pendingRepairRoot(repairOfferId) != null
        }.getOrNull()
        throw AssertionError(
            "$label did not expose one stable refreshed API before its deadline. " +
                "sawChangedBaseUrl=$sawChangedBaseUrl lastLifecycle=$lastLifecycle " +
                "lastAvailability=$lastAvailability lastIssue=$lastIssue " +
                "lastSharePhase=$lastSharePhase lastFolderState=$lastFolderState " +
                "lastAccessUnavailable=$lastAccessUnavailable persistedPermission=$persistedPermission " +
                "pendingRepair=$pendingRepair lastErrorClass=$lastErrorClass lastHttpStatus=$lastHttpStatus",
        )
    }

    private fun awaitSharedSettings(
        source: NodeConnection,
        destination: NodeConnection,
        expected: FolderLinkSettings,
        expectedRevision: Long? = null,
    ): FolderLinkSettingsState = awaitValue("shared link settings") {
        val sourceState = sourceShare(source).linkSettings
        val destinationState = client.folderSyncStatus(destination.baseUrl, destination.token).shares
            .singleOrNull { it.incoming && it.offerId == offerId }
            ?.linkSettings
        sourceState?.takeIf {
            it.confirmed && it.pendingChange == null && it.conflictedChange == null &&
                it.settings == expected &&
                (expectedRevision == null || it.revision == expectedRevision) &&
                destinationState?.revision == it.revision && destinationState.confirmed &&
                destinationState.pendingChange == null && destinationState.conflictedChange == null &&
                destinationState.settings == expected
        }
    }

    @Suppress("DEPRECATION")
    private fun chooseSystemFolder(
        label: String,
        fixture: String,
        folder: String,
        selectedUris: MutableList<Uri>,
    ): SafFolderGrant {
        val store = SafFolderGrantStore(context)
        val before = store.records().mapTo(mutableSetOf()) { it.id }
        println("COVALENT_SYSTEM_PICKER_REQUEST=$label")
        clickScreenText(context.getString(R.string.folder_sync_use_folder))
        selectSystemPickerFolder(fixture, folder)
        return awaitValue("$label system folder grant") {
            instrumentation.waitForIdleSync()
            compose.waitForIdle()
            store.records().singleOrNull { it.id !in before }?.also { selectedUris += it.treeUri }
        }.also { assertExactPickerTree(it, fixture, folder) }
    }

    private fun rechooseSystemFolder(
        label: String,
        fixture: String,
        folder: String,
        expected: SafFolderGrant,
        selectedUris: MutableList<Uri>,
    ): SafFolderGrant {
        val store = SafFolderGrantStore(context)
        println("COVALENT_SYSTEM_PICKER_REQUEST=$label")
        clickScreenText(context.getString(R.string.folder_sync_use_folder))
        selectSystemPickerFolder(fixture, folder)
        return awaitValue("$label restored system folder grant") {
            instrumentation.waitForIdleSync()
            compose.waitForIdle()
            store.records().singleOrNull { it.id == expected.id && it.treeUri == expected.treeUri }
                ?.takeIf { grant ->
                    context.contentResolver.persistedUriPermissions.any {
                        it.uri == grant.treeUri && it.isReadPermission && it.isWritePermission
                    }
                }
                ?.also { selectedUris += it.treeUri }
        }.also { assertExactPickerTree(it, fixture, folder) }
    }

    private fun prepareSystemPickerFixture(fixture: String) {
        check(SAF_PICKER_COMPONENT.matches(fixture))
        shell("mkdir -p /sdcard/Download/$fixture/source /sdcard/Download/$fixture/destination")
    }

    private fun removeSystemPickerFixture(fixture: String) {
        check(SAF_PICKER_COMPONENT.matches(fixture))
        shell("rm -rf /sdcard/Download/$fixture")
    }

    private fun selectSystemPickerFolder(fixture: String, folder: String) {
        check(SAF_PICKER_COMPONENT.matches(fixture) && SAF_PICKER_COMPONENT.matches(folder))
        val pickerWaitStarted = System.nanoTime()
        var lastRootCategory = "unavailable"
        try {
            await("system folder picker") {
                val rootPackage = instrumentation.uiAutomation.rootInActiveWindow?.packageName?.toString()
                lastRootCategory = pickerPackageCategory(rootPackage)
                rootPackage == DOCUMENTS_UI_PACKAGE
            }
        } catch (failure: AssertionError) {
            val elapsedMillis = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - pickerWaitStarted)
            val currentRootCategory = pickerPackageCategory(runCatching {
                instrumentation.uiAutomation.rootInActiveWindow?.packageName?.toString()
            }.getOrNull())
            throw AssertionError(
                "${failure.message} elapsedMillis=$elapsedMillis " +
                    "lastRoot=$lastRootCategory currentRoot=$currentRootCategory " +
                    pickerWindowDiagnostic(),
                failure,
            )
        }
        if (systemPickerHasText("Files in source") || systemPickerHasText("Files in destination")) {
            shell("input keyevent KEYCODE_BACK")
            await("system picker returned to owned fixture") { systemPickerHasText(folder) }
        }
        await("system picker owned fixture") {
            if (systemPickerHasText(fixture)) {
                true
            } else {
                clickSystemPickerTextIfClickable("Download")
                Thread.sleep(SYSTEM_PICKER_NAVIGATION_MILLIS)
                false
            }
        }
        clickSystemPickerText(fixture)
        Thread.sleep(SYSTEM_PICKER_NAVIGATION_MILLIS)
        clickSystemPickerText(folder)
        Thread.sleep(SYSTEM_PICKER_NAVIGATION_MILLIS)
        clickSystemPickerText("USE THIS FOLDER")
        clickSystemPickerText("ALLOW")
    }

    private fun pickerPackageCategory(packageName: String?): String = when (packageName) {
        null -> "unavailable"
        DOCUMENTS_UI_PACKAGE -> "DocumentsUI"
        context.packageName -> "target"
        instrumentation.context.packageName -> "test-host"
        else -> "other"
    }

    private fun pickerWindowDiagnostic(): String = runCatching {
        val windows = instrumentation.uiAutomation.windows
        val categories = windows.take(MAX_DIAGNOSTIC_WINDOWS).joinToString(prefix = "[", postfix = "]") {
            val category = pickerPackageCategory(it.root?.packageName?.toString())
            "$category(active=${it.isActive},focused=${it.isFocused})"
        }
        "accessibilityWindowCount=${windows.size} accessibilityWindows=$categories"
    }.getOrElse { "accessibilityWindowCount=unavailable accessibilityWindows=[]" }

    private fun assertExactPickerTree(grant: SafFolderGrant, fixture: String, folder: String) {
        assertEquals(
            "primary:Download/$fixture/$folder",
            DocumentsContract.getTreeDocumentId(grant.treeUri),
        )
    }

    private fun clickSystemPickerText(text: String) {
        await("system picker action $text") { clickSystemPickerTextIfClickable(text) }
    }

    private fun systemPickerHasText(text: String): Boolean =
        instrumentation.uiAutomation.rootInActiveWindow
            ?.findAccessibilityNodeInfosByText(text)
            ?.any { it.isVisibleToUser && it.text?.toString() == text } == true

    private fun clickSystemPickerTextIfClickable(text: String): Boolean {
        val root = instrumentation.uiAutomation.rootInActiveWindow ?: return false
        val node = root.findAccessibilityNodeInfosByText(text)
            .asSequence()
            .filter { it.isVisibleToUser && it.text?.toString() == text }
            .firstOrNull() ?: return false
        val bounds = Rect()
        node.getBoundsInScreen(bounds)
        if (bounds.isEmpty) return false
        shell("input tap ${bounds.centerX()} ${bounds.centerY()}")
        return true
    }

    private fun registerSecondNodeGrant(connection: NodeConnection, grant: SafFolderGrant) {
        val credential = ByteArray(24).also(SecureRandom()::nextBytes).let { random ->
            try {
                Base64.getUrlEncoder().withoutPadding().encodeToString(random)
            } finally {
                random.fill(0)
            }
        }
        val server = SafWebDavServer(context, grant.treeUri, "covalent-test-user", credential)
        val endpoint = server.start()
        secondSafServers += server
        assertEquals(
            NativeFolderGrantResult.OK,
            CovalentNative.registerFolderGrant(
                secondHandle,
                grant.id.toString(),
                endpoint.port,
                endpoint.username,
                endpoint.password,
            ),
        )
        client.retryFolderSync(connection.baseUrl, connection.token)
    }

    private fun writeSafFile(grant: SafFolderGrant, name: String, bytes: ByteArray) {
        val root = checkNotNull(DocumentFile.fromTreeUri(context, grant.treeUri))
        val file = root.findFile(name) ?: checkNotNull(root.createFile("application/octet-stream", name))
        checkNotNull(context.contentResolver.openOutputStream(file.uri, "rwt")).use { output ->
            output.write(bytes)
        }
    }

    private fun readSafFile(grant: SafFolderGrant, name: String): ByteArray? {
        val root = DocumentFile.fromTreeUri(context, grant.treeUri) ?: return null
        val file = root.findFile(name) ?: return null
        return context.contentResolver.openInputStream(file.uri)?.use { input ->
            input.readBounded(MAX_TEST_FILE_BYTES)
        }
    }

    private fun deleteSafFile(grant: SafFolderGrant, name: String): Boolean {
        val root = DocumentFile.fromTreeUri(context, grant.treeUri) ?: return false
        return root.findFile(name)?.delete() == true
    }

    private fun awaitSafFile(label: String, grant: SafFolderGrant, name: String, expected: ByteArray) {
        await(label, TRANSFER_TIMEOUT_MILLIS) {
            readSafFile(grant, name)?.contentEquals(expected) == true
        }
    }

    private fun assertSafFileAbsentFor(grant: SafFolderGrant, name: String, durationMillis: Long) {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(durationMillis)
        while (System.nanoTime() < deadline) {
            assertTrue("A Manual SAF link copied before Run Now", readSafFile(grant, name) == null)
            Thread.sleep(POLL_MILLIS)
        }
    }

    private fun startSecondNode(
        packageValue: PackagedSyncEnginePackage.Verified,
        peerListenerPort: Int = 0,
        folderSyncListenerPort: Int? = null,
        runId: String,
    ): NodeConnection {
        val data = secondDataDirectory(runId)
        check(data.parentFile == context.noBackupFilesDir.canonicalFile)
        if (secondKeyEncryptionKey == null) {
            check(data.mkdir())
        } else {
            check(data.isDirectory)
        }
        secondData = data
        val random = SecureRandom()
        val token = secondApiToken ?: ByteArray(32).also(random::nextBytes).let { tokenRandom ->
            Base64.getUrlEncoder().withoutPadding().encodeToString(tokenRandom)
                .also { tokenRandom.fill(0) }
        }.also { secondApiToken = it }
        val key = secondKeyEncryptionKey ?: ByteArray(32).also(random::nextBytes)
            .also { secondKeyEncryptionKey = it }
        val syncListenerPort = folderSyncListenerPort
            ?: secondFolderSyncListenerPort
            ?: FolderSyncInstrumentationBridge.reserveEphemeralListenerPort()
        secondFolderSyncListenerPort = syncListenerPort
        val response = CovalentNative.start(
            dataDirectory = data.path,
            deviceName = "API 37 isolated peer",
            lanDiscoveryEnabled = false,
            apiToken = token.toByteArray(StandardCharsets.US_ASCII),
            keyEncryptionKey = key.copyOf(),
            keyVersion = 1,
            maximumTotalBytes = 512L * 1024L * 1024L,
            freeSpaceReserveBytes = 0,
            keyProtectionLevel = KeyProtectionLevel.SOFTWARE,
            syncEngine = packageValue,
            backupProviderEnabled = false,
            folderSyncListenerPort = syncListenerPort,
            peerListenerPort = peerListenerPort,
        )
        assertTrue("The isolated JNI node must start", response.ok)
        secondHandle = checkNotNull(response.handle)
        return NodeConnection(checkNotNull(response.apiBaseUrl), token)
    }

    private fun reserveEphemeralPeerPort(excludedPort: Int): Int {
        repeat(16) {
            val candidate = DatagramSocket(0).use { it.localPort }
            if (candidate != excludedPort) return candidate
        }
        error("A distinct test-owned peer port could not be reserved.")
    }

    private fun reserveEphemeralWorkerPort(excludedPort: Int): Int {
        repeat(16) {
            val candidate = FolderSyncInstrumentationBridge.reserveEphemeralListenerPort()
            if (candidate != excludedPort) return candidate
        }
        error("A distinct test-owned worker port could not be reserved.")
    }

    private fun canBindPeerPort(port: Int): Boolean = runCatching {
        DatagramSocket(null).use { socket ->
            socket.reuseAddress = false
            socket.bind(InetSocketAddress(InetAddress.getByAddress(byteArrayOf(0, 0, 0, 0)), port))
        }
        true
    }.getOrDefault(false)

    private fun canBindWorkerPort(port: Int): Boolean = runCatching {
        ServerSocket().use { socket ->
            socket.reuseAddress = true
            socket.bind(InetSocketAddress(InetAddress.getByAddress(byteArrayOf(0, 0, 0, 0)), port))
        }
        true
    }.getOrDefault(false)

    private fun forgetSecondNodeSecrets() {
        secondKeyEncryptionKey?.fill(0)
        secondKeyEncryptionKey = null
        secondApiToken = null
        secondFolderSyncListenerPort = null
    }

    private fun pairUsingPhoneUi(
        connectionA: NodeConnection,
        connectionB: NodeConnection,
        peerAddressB: String,
    ) {
        compose.onNodeWithTag("folder-sync-list")
            .performScrollToNode(hasTestTag("folder-pair.address"))
        compose.onNodeWithTag("folder-pair.address").performTextInput(peerAddressB)
        val start = compose.onNodeWithTag("folder-pair.start")
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { start.assertIsEnabled(); true }.getOrDefault(false)
        }
        start.performClick()
        val incoming = awaitValue("incoming network pairing") {
            client.pendingNetworkPairings(connectionB.baseUrl, connectionB.token)
                .singleOrNull { it.direction == NetworkPairingDirection.INCOMING }
        }
        val outgoing = awaitValue("phone pairing request") {
            client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
                .singleOrNull { it.pairingId == incoming.pairingId }
        }
        assertEquals(outgoing.authenticationString, incoming.authenticationString)
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching {
                compose.onNodeWithTag("folder-sync-list")
                    .performScrollToNode(hasTestTag("pair.authenticationString"))
                compose.onNodeWithTag("pair.authenticationString").assertIsDisplayed()
                true
            }.getOrDefault(false)
        }
        compose.onNodeWithTag("pair.authenticationString")
            .assertContentDescriptionEquals(incoming.authenticationString.replace('-', ' '))
        clickScreenText(context.getString(R.string.folder_sync_confirm_device))
        await("phone records local SAS confirmation") {
            client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
                .singleOrNull { it.pairingId == incoming.pairingId }
                ?.state == NetworkPairingState.AWAITING_PEER_CONFIRMATION
        }
        val second = client.confirmNetworkPairing(
            connectionB.baseUrl,
            connectionB.token,
            incoming.pairingId,
            incoming.authenticationString,
        )
        assertEquals(NetworkPairingState.COMPLETE, second.state)
        val completed = awaitValue("initiator observes completed pairing") {
            client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
                .singleOrNull { it.pairingId == incoming.pairingId }
                ?.takeIf { it.state == NetworkPairingState.COMPLETE }
        }
        assertEquals(peerAddressB, checkNotNull(completed.peerTransport).address)
        val expectedPeerId = checkNotNull(completed.peerTransport).peerId
        visibleScreenText(context.getString(R.string.folder_sync_pair_complete)) {
            val requests = client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
            val syncStatus = client.folderSyncStatus(connectionA.baseUrl, connectionA.token)
            val peers = syncStatus.peers
            "apiPairingCount=${requests.size} " +
                "apiCompletedPairingCount=${requests.count { it.state == NetworkPairingState.COMPLETE }} " +
                "apiPeerCount=${peers.size} expectedPeerPresent=${peers.any { it.peerId == expectedPeerId }} " +
                "apiAvailability=${syncStatus.availability} apiLifecycle=${syncStatus.lifecycle} " +
                "apiIssue=${syncStatus.issue}"
        }.assertIsDisplayed()
        clickScreenText(context.getString(R.string.action_done))
        await("completed phone pairing request dismissed") {
            client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
                .none { it.pairingId == incoming.pairingId }
        }
    }

    private fun advertisedGuestPeerAddress(peerPort: Int): String {
        val inContainer = File("/.dockerenv").exists() ||
            File("/run/.containerenv").exists() ||
            runCatching { File("/proc/1/cgroup").readText() }.getOrNull()?.let { cgroup ->
                cgroup.contains("/docker/") ||
                    cgroup.contains("/containerd/") ||
                    cgroup.contains("/kubepods")
            } == true
        val candidates = mutableListOf<Pair<Int, InetAddress>>()
        val interfaces = NetworkInterface.getNetworkInterfaces()
            ?: error("The hosted emulator exposes no network interfaces.")
        while (interfaces.hasMoreElements()) {
            val networkInterface = interfaces.nextElement()
            if (networkInterface.isLoopback) continue
            val addresses = networkInterface.inetAddresses
            while (addresses.hasMoreElements()) {
                val address = addresses.nextElement()
                advertisedAddressClass(address)?.let { addressClass ->
                    if (addressClass != ADDRESS_CLASS_CONTAINER_BRIDGE || !inContainer) {
                        candidates += addressClass to address
                    }
                }
            }
        }
        val address = candidates.minWithOrNull { left, right ->
            compareValues(left.first, right.first)
                .takeIf { it != 0 }
                ?: compareValues(left.second is Inet6Address, right.second is Inet6Address)
                    .takeIf { it != 0 }
                ?: compareUnsignedAddressBytes(left.second.address, right.second.address)
        }?.second ?: error("The hosted emulator exposes no advertisable guest address.")
        val host = checkNotNull(address.hostAddress).substringBefore('%')
        return if (address is Inet6Address) "[$host]:$peerPort" else "$host:$peerPort"
    }

    private fun advertisedAddressClass(address: InetAddress): Int? = when (address) {
        is Inet4Address -> {
            val bytes = address.address.map(Byte::toUByte)
            val first = bytes[0].toInt()
            val second = bytes[1].toInt()
            when {
                address.isLoopbackAddress || address.isLinkLocalAddress ||
                    address.isAnyLocalAddress || address.isMulticastAddress ||
                    bytes.all { it == UByte.MAX_VALUE } -> null
                first == 192 && second == 0 && bytes[2].toInt() == 2 -> null
                first == 198 && second == 51 && bytes[2].toInt() == 100 -> null
                first == 203 && second == 0 && bytes[2].toInt() == 113 -> null
                first == 100 && second in 64..127 -> ADDRESS_CLASS_TAILNET
                first == 172 && second in 16..31 -> ADDRESS_CLASS_CONTAINER_BRIDGE
                address.isSiteLocalAddress -> ADDRESS_CLASS_PRIVATE_LAN
                else -> null
            }
        }
        is Inet6Address -> {
            val bytes = address.address.map(Byte::toUByte)
            when {
                address.isLoopbackAddress || address.isAnyLocalAddress ||
                    address.isMulticastAddress || address.isLinkLocalAddress -> null
                bytes[0].toInt() == 0xfd && bytes[1].toInt() == 0x7a &&
                    bytes[2].toInt() == 0x11 && bytes[3].toInt() == 0x5c &&
                    bytes[4].toInt() == 0xa1 && bytes[5].toInt() == 0xe0 -> ADDRESS_CLASS_TAILNET
                bytes[0].toInt() and 0xfe == 0xfc -> ADDRESS_CLASS_PRIVATE_LAN
                else -> null
            }
        }
        else -> null
    }

    private fun compareUnsignedAddressBytes(left: ByteArray, right: ByteArray): Int {
        left.indices.forEach { index ->
            compareValues(left[index].toUByte(), right[index].toUByte())
                .takeIf { it != 0 }
                ?.let { return it }
        }
        return 0
    }

    private fun EmbeddedNodeManager.liveConnection(): NodeConnection? {
        val value = localConnectionForFolderSync() ?: return null
        return value.takeIf { runCatching { client.status(it.baseUrl) }.isSuccess }
    }

    private fun shell(command: String): String {
        val descriptor = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
            val bytes = input.readBounded(MAX_SHELL_OUTPUT_BYTES)
            bytes.toString(StandardCharsets.UTF_8).trim()
        }
    }

    private fun InputStream.readBounded(maximumBytes: Int): ByteArray {
        val output = java.io.ByteArrayOutputStream(maximumBytes.coerceAtMost(8 * 1_024))
        val buffer = ByteArray(4 * 1_024)
        var retained = 0
        while (true) {
            val count = read(buffer)
            if (count < 0) break
            check(count > 0)
            retained += count
            check(retained <= maximumBytes)
            output.write(buffer, 0, count)
        }
        return output.toByteArray()
    }

    private fun exactHelperCounts(): HelperCounts {
        val engine = checkNotNull(installedEngine)
        return HelperCounts(
            workers = exactExecutableProcesses(File(engine.workerPath)).size,
            guardians = exactExecutableProcesses(File(engine.guardianPath)).size,
        )
    }

    private fun exactExecutableProcesses(expectedFile: File): List<Int> {
        val expected = expectedFile.canonicalPath
        val result = mutableListOf<Int>()
        val seen = mutableSetOf<Int>()
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(PROCESS_SCAN_SECONDS)
        var entries = 0
        Files.newDirectoryStream(File("/proc").toPath()).use { stream: DirectoryStream<Path> ->
            for (path in stream) {
                check(++entries <= MAX_PROC_ENTRIES && System.nanoTime() < deadline) {
                    "The bounded process observation exceeded its limit."
                }
                val name = path.fileName.toString()
                if (!name.isAsciiPid()) continue
                val pid = name.toIntOrNull() ?: continue
                if (!seen.add(pid)) continue
                try {
                    val first = File(Os.readlink("/proc/$pid/exe")).canonicalPath
                    if (first != expected) continue
                    val second = File(Os.readlink("/proc/$pid/exe")).canonicalPath
                    if (second == expected) result += pid
                } catch (_: ErrnoException) {
                    // An exiting process is not a stable exact-executable match.
                } catch (_: IOException) {
                    // The proc entry or executable can disappear between the two reads.
                }
            }
        }
        return result
    }

    private fun String.isAsciiPid(): Boolean = isNotEmpty() && length <= 10 && all { it in '0'..'9' }

    private fun awaitExactHelperCounts(workers: Int, guardians: Int) {
        await("exact helper process count") { exactHelperCounts() == HelperCounts(workers, guardians) }
    }

    private fun requireRunId(value: String?): String {
        if (value == null || !RUN_ID.matches(value)) {
            throw AssertionError("The host runner did not provide a canonical folder journey run ID.")
        }
        return value
    }

    private fun secondDataDirectory(runId: String): File =
        File(context.noBackupFilesDir.canonicalFile, "$SECOND_DATA_PREFIX${requireRunId(runId)}")
            .absoluteFile.also { check(it.parentFile == context.noBackupFilesDir.canonicalFile) }

    private fun deleteTreeWithoutFollowingLinks(root: File) {
        val rootPath = root.toPath()
        if (!Files.exists(rootPath, java.nio.file.LinkOption.NOFOLLOW_LINKS)) return
        var entries = 0
        Files.walkFileTree(
            rootPath,
            emptySet<java.nio.file.FileVisitOption>(),
            MAX_CLEANUP_DEPTH,
            object : java.nio.file.SimpleFileVisitor<Path>() {
                private fun charge() {
                    check(++entries <= MAX_CLEANUP_ENTRIES) {
                        "The test-owned cleanup tree exceeded its entry bound."
                    }
                }

                override fun preVisitDirectory(
                    directory: Path,
                    attributes: java.nio.file.attribute.BasicFileAttributes,
                ): java.nio.file.FileVisitResult {
                    charge()
                    return java.nio.file.FileVisitResult.CONTINUE
                }

                override fun visitFile(
                    file: Path,
                    attributes: java.nio.file.attribute.BasicFileAttributes,
                ): java.nio.file.FileVisitResult {
                    charge()
                    Files.delete(file)
                    return java.nio.file.FileVisitResult.CONTINUE
                }

                override fun postVisitDirectory(
                    directory: Path,
                    error: java.io.IOException?,
                ): java.nio.file.FileVisitResult {
                    if (error != null) throw error
                    Files.delete(directory)
                    return java.nio.file.FileVisitResult.CONTINUE
                }
            },
        )
    }

    private fun await(label: String, timeoutMillis: Long = DEFAULT_TIMEOUT_MILLIS, condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMillis)
        var last: Throwable? = null
        while (System.nanoTime() < deadline) {
            try {
                if (condition()) return
            } catch (error: Exception) {
                last = error
            }
            Thread.sleep(POLL_MILLIS)
        }
        throw AssertionError("$label did not complete before its deadline.", last)
    }

    private fun <T> awaitValue(label: String, condition: () -> T?): T {
        var value: T? = null
        await(label) { condition()?.also { value = it } != null }
        return checkNotNull(value)
    }

    private data class HelperCounts(val workers: Int, val guardians: Int)

    private companion object {
        const val SAF_FOLDER_LABEL = "API 37 SAF manual journey"
        const val SAF_FIXTURE_PREFIX = "CovalentFinal-"
        const val DOCUMENTS_UI_PACKAGE = "com.google.android.documentsui"
        const val SECOND_DATA_PREFIX = "journey-"
        const val ADDRESS_CLASS_PRIVATE_LAN = 0
        const val ADDRESS_CLASS_TAILNET = 1
        const val ADDRESS_CLASS_CONTAINER_BRIDGE = 2
        val RUN_ID = Regex("[0-9a-f]{32}")
        val SAF_PICKER_COMPONENT = Regex("[A-Za-z0-9-]{1,96}")
        val SAF_MANUAL_CONTENT = "api37-saf-manual-run\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_ADDRESS_CONTENT = "api37-saf-after-address-refresh\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_RESTART_CONTENT = "api37-saf-after-restart\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_RECOVERY_CONTENT = "api37-saf-after-grant-repair\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_CONTINUOUS_CONTENT = "api37-saf-continuous\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_PAUSED_CONTENT = "api37-saf-held-while-paused\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_REVERSE_CONTENT = "api37-saf-destination-only\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_RESTORE_CONTENT = "api37-saf-restore-destination\n".toByteArray(StandardCharsets.UTF_8)
        val SAF_PROPAGATE_CONTENT = "api37-saf-propagate-source\n".toByteArray(StandardCharsets.UTF_8)
        const val SAF_UNICODE_NAME = "Grüße.txt"
        val SAF_UNICODE_CONTENT = "grüße aus Android\n".toByteArray(StandardCharsets.UTF_8)
        const val POLL_MILLIS = 250L
        const val SYSTEM_PICKER_NAVIGATION_MILLIS = 1_000L
        const val STABLE_API_MILLIS = 5_000L
        const val DEFAULT_TIMEOUT_MILLIS = 90_000L
        const val MAX_DIAGNOSTIC_NODES = 2
        const val MAX_DIAGNOSTIC_WINDOWS = 8
        const val TRANSFER_TIMEOUT_MILLIS = 120_000L
        const val NEGATIVE_WINDOW_MILLIS = 7_000L
        const val PROCESS_SCAN_SECONDS = 5L
        const val MAX_PROC_ENTRIES = 4_096
        const val MAX_CLEANUP_ENTRIES = 100_000
        const val MAX_CLEANUP_DEPTH = 64
        const val MAX_TEST_FILE_BYTES = 64 * 1_024
        const val MAX_SHELL_OUTPUT_BYTES = 16 * 1_024
    }
}
