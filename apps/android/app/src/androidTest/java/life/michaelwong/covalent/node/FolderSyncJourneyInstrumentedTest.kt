package life.michaelwong.covalent.node

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Process
import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.assertContentDescriptionEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isDialog
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performTextInput
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.FileInputStream
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
import java.nio.file.StandardOpenOption
import java.security.SecureRandom
import java.util.Base64
import java.util.concurrent.TimeUnit
import life.michaelwong.covalent.BuildConfig
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncIssue
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.NetworkPairingDirection
import life.michaelwong.covalent.model.NetworkPairingState
import life.michaelwong.covalent.model.NodeConnection
import life.michaelwong.covalent.model.PeerConnectionFreshness
import life.michaelwong.covalent.model.PeerConnectionState
import life.michaelwong.covalent.sync.FolderSyncSpecialAccess
import life.michaelwong.covalent.sync.RawFolderAccess
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import life.michaelwong.covalent.ui.FolderSyncScreen
import life.michaelwong.covalent.ui.theme.CovalentTheme
import org.junit.Rule
import org.junit.Test

/**
 * Hosted API-37 proof of the real Android service, JNI, guardian, and packaged workers.
 *
 * The host runs this one named test in three separate instrumentation processes. Android kills an
 * app when its UID-wide all-files app-op changes, so changing that permission from this test would
 * kill the assertion code itself. A private synchronous receipt carries only bounded, nonsecret
 * fixture identity between setup, denied, and restored phases.
 */
class FolderSyncJourneyInstrumentedTest {
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
    private var fixtureRoot: File? = null
    private var secondData: File? = null

    @Test
    fun nativeFolderScreenSyncsBothWaysAcrossPauseRestartAccessLossAndRemoval() {
        assertTrue("This destructive permission fixture runs only on the hosted emulator", shell("getprop ro.kernel.qemu") == "1")
        assertTrue(BuildConfig.DEBUG)
        assertTrue(BuildConfig.COVALENT_SYNC_ENGINE_PACKAGED)
        assertTrue(CovalentNative.isAvailable)
        val arguments = InstrumentationRegistry.getArguments()
        val runId = requireRunId(arguments.getString(JOURNEY_RUN_ID_ARGUMENT))
        val phase = arguments.getString(JOURNEY_PHASE_ARGUMENT)
            ?: throw AssertionError("The host runner did not select an Android folder journey phase.")
        val manager = EmbeddedNodeManager(context)

        when (phase) {
            PHASE_SETUP -> runSetupPhase(manager, runId)
            PHASE_DENIED -> runDeniedPhase(
                manager,
                runId,
                requirePriorProcessId(arguments.getString(JOURNEY_PRIOR_PID_ARGUMENT)),
            )
            PHASE_RESTORED -> runRestoredPhase(manager, runId)
            else -> fail("The host runner selected an unknown Android folder journey phase.")
        }
    }

    private fun runSetupPhase(manager: EmbeddedNodeManager, runId: String) {
        assertEquals(
            PackageManager.PERMISSION_GRANTED,
            context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK),
        )
        await("host-granted all-files access") { FolderSyncSpecialAccess.granted() }
        cleanupStaleFixture(manager)
        var preserveForPermissionKill = false
        try {
            assertFalse("The hosted app fixture must start without folder-sync demand", manager.folderSyncRequested())
            val packageValue = FolderSyncInstrumentationBridge.isolatedPackage(context)
            installedEngine = packageValue.engine
            val roots = createExternalFixture(runId)
            val rootA = RawFolderAccess(context).select(roots.first.path)
            val rootB = RawFolderAccess(context).select(roots.second.path)
            assertTrue(manager.enableFolderSyncHost())
            val connectionA = awaitValue("service node API") { manager.liveConnection() }
            val identityA = client.transportIdentity(connectionA.baseUrl, connectionA.token)
            var connectionB = startSecondNode(packageValue)
            val identityB = client.transportIdentity(connectionB.baseUrl, connectionB.token)
            assertTrue(identityA.deviceId != identityB.deviceId)

            val forward = "api37-forward-before-offer\n".toByteArray(StandardCharsets.UTF_8)
            writeNew(File(rootA, "forward.txt"), forward)
            compose.setContent {
                CovalentTheme { if (showScreen.value) FolderSyncScreen(manager) }
            }
            pairUsingPhoneUi(
                connectionA,
                connectionB,
                advertisedGuestPeerAddress(identityB.peerPort),
            )
            clickScreenText(context.getString(R.string.folder_sync_storage_roots))
            val volume = RawFolderAccess(context).roots().first()
            clickScreenText(volume.name.ifBlank { volume.absolutePath })
            clickScreenText(checkNotNull(fixtureRoot).name)
            clickScreenText(roots.first.name)
            clickScreenText(context.getString(R.string.folder_sync_use_folder))
            val labelMatcher = hasSetTextAction() and hasText(context.getString(R.string.folder_sync_label))
            compose.onNodeWithTag("folder-sync-list").performScrollToNode(labelMatcher)
            compose.onNode(labelMatcher).performTextInput(FOLDER_LABEL)
            clickScreenText("API 37 isolated peer")
            clickScreenText(context.getString(R.string.folder_sync_offer))
            offerId = awaitValue("native folder offer recorded") {
                client.folderSyncStatus(connectionA.baseUrl, connectionA.token).shares
                    .singleOrNull { !it.incoming && it.peerId == identityB.deviceId && it.label == FOLDER_LABEL }
                    ?.offerId
            }
            await("incoming folder offer") {
                client.folderSyncStatus(connectionB.baseUrl, connectionB.token).shares.any {
                    it.offerId == offerId && it.incoming && it.phase == FolderSharePhase.OFFERED
                }
            }
            client.acceptFolder(connectionB.baseUrl, connectionB.token, checkNotNull(offerId), rootB)
            awaitFile("forward transfer", File(rootB, "forward.txt"), forward)
            awaitExactHelperCounts(workers = 2, guardians = 2)

            val reverse = "api37-reverse-before-restart\n".toByteArray(StandardCharsets.UTF_8)
            writeNew(File(rootB, "reverse.txt"), reverse)
            awaitFile("reverse transfer", File(rootA, "reverse.txt"), reverse)

            clickScreenText(context.getString(R.string.action_refresh_backups))
            clickScreenText(context.getString(R.string.action_pause))
            await("paused service worker stop") {
                exactHelperCounts() == HelperCounts(workers = 1, guardians = 1)
            }
            overwrite(File(rootA, "forward.txt"), PAUSED_CONTENT)
            assertFileUnchangedFor(File(rootB, "forward.txt"), forward, NEGATIVE_WINDOW_MILLIS)
            clickScreenText(context.getString(R.string.action_resume))
            awaitFile("resumed transfer", File(rootB, "forward.txt"), PAUSED_CONTENT)
            awaitExactHelperCounts(workers = 2, guardians = 2)

            val oldWorkerPort = checkNotNull(secondFolderSyncListenerPort)
            val movedPeerPort = reserveEphemeralPeerPort(identityB.peerPort)
            val movedWorkerPort = reserveEphemeralWorkerPort(oldWorkerPort)
            assertTrue(CovalentNative.stop(secondHandle).ok)
            secondHandle = 0
            awaitExactHelperCounts(workers = 1, guardians = 1)
            await("old second-node listeners released") {
                canBindPeerPort(identityB.peerPort) && canBindWorkerPort(oldWorkerPort)
            }
            connectionB = startSecondNode(packageValue, movedPeerPort, movedWorkerPort)
            val movedIdentity = client.transportIdentity(connectionB.baseUrl, connectionB.token)
            assertEquals(identityB.deviceId, movedIdentity.deviceId)
            assertEquals(identityB.certificateFingerprint, movedIdentity.certificateFingerprint)
            assertEquals(identityB.certificateDer, movedIdentity.certificateDer)
            assertEquals(movedPeerPort, movedIdentity.peerPort)
            assertTrue(identityB.peerPort != movedIdentity.peerPort)
            assertEquals(movedWorkerPort, secondFolderSyncListenerPort)
            assertTrue(oldWorkerPort != movedWorkerPort)
            awaitExactHelperCounts(workers = 2, guardians = 2)

            val movedPeerAddress = advertisedGuestPeerAddress(movedPeerPort)
            val addressButtonTag = "folder-peer-address-${identityB.deviceId}"
            compose.onNodeWithTag("folder-sync-list")
                .performScrollToNode(hasTestTag(addressButtonTag))
            clickScreenTag(addressButtonTag)
            compose.onNodeWithTag("folder-peer-address-input")
                .performTextInput(movedPeerAddress)
            clickScreenTag("folder-peer-address-confirm")
            await("native address refresh becomes authoritative") {
                client.folderSyncStatus(connectionA.baseUrl, connectionA.token).peers
                    .singleOrNull { it.peerId == identityB.deviceId }
                    ?.address == movedPeerAddress
            }

            writeNew(File(rootA, "after-address-forward.txt"), AFTER_ADDRESS_FORWARD_CONTENT)
            awaitFile(
                "forward transfer after native address refresh",
                File(rootB, "after-address-forward.txt"),
                AFTER_ADDRESS_FORWARD_CONTENT,
            )
            writeNew(File(rootB, "after-address-reverse.txt"), AFTER_ADDRESS_REVERSE_CONTENT)
            awaitFile(
                "reverse transfer after native address refresh",
                File(rootA, "after-address-reverse.txt"),
                AFTER_ADDRESS_REVERSE_CONTENT,
            )

            stopServiceAndAwaitWorkers(checkNotNull(manager.localConnectionForFolderSync()), remainingWorkers = 1)
            manager.reconnectIfEnabled()
            val restartedA = awaitValue("cold restarted service node API") { manager.liveConnection() }
            val restartedIdentity = client.transportIdentity(restartedA.baseUrl, restartedA.token)
            assertEquals(identityA.deviceId, restartedIdentity.deviceId)
            assertEquals(identityA.peerPort, restartedIdentity.peerPort)
            assertEquals(identityA.certificateFingerprint, restartedIdentity.certificateFingerprint)
            assertEquals(identityA.certificateDer, restartedIdentity.certificateDer)
            awaitExactHelperCounts(workers = 2, guardians = 2)
            writeNew(File(rootA, "after-address-cold-forward.txt"), AFTER_ADDRESS_COLD_FORWARD_CONTENT)
            awaitFile(
                "persisted refreshed route after cold service restart",
                File(rootB, "after-address-cold-forward.txt"),
                AFTER_ADDRESS_COLD_FORWARD_CONTENT,
            )
            writeNew(File(rootB, "after-restart.txt"), AFTER_RESTART_CONTENT)
            awaitFile("transfer after cold service restart", File(rootA, "after-restart.txt"), AFTER_RESTART_CONTENT)

            // Leave exactly the production service worker alive. The host now
            // launches the ordinary activity, proves that persisted startup has
            // restored this one worker, and changes the UID app-op outside this
            // process. Android intentionally kills this process for that change.
            assertTrue(CovalentNative.stop(secondHandle).ok)
            secondHandle = 0
            forgetSecondNodeSecrets()
            awaitExactHelperCounts(workers = 1, guardians = 1)
            persistReceipt(
                JourneyReceipt(
                    runId = runId,
                    stage = RECEIPT_SETUP,
                    offerId = checkNotNull(offerId),
                    peerId = identityB.deviceId,
                    setupProcessId = Process.myPid(),
                ),
            )
            instrumentation.runOnMainSync { showScreen.value = false }
            preserveForPermissionKill = true
        } finally {
            if (!preserveForPermissionKill) cleanup(manager, runId)
        }
    }

    private fun runDeniedPhase(manager: EmbeddedNodeManager, runId: String, priorProcessId: Int) {
        val receipt = readReceipt(runId, RECEIPT_SETUP)
        offerId = receipt.offerId
        installedEngine = FolderSyncInstrumentationBridge.isolatedPackage(context).engine
        fixtureRoot = fixtureDirectory(runId)
        secondData = secondDataDirectory(runId)
        assertFalse(FolderSyncSpecialAccess.granted())
        assertTrue(
            "The denied phase must run after the host observed the old process die",
            priorProcessId != Process.myPid(),
        )
        assertTrue(
            "The setup instrumentation process must not survive the host-observed permission kill",
            receipt.setupProcessId != Process.myPid(),
        )
        awaitExactHelperCounts(workers = 0, guardians = 0)
        var preserveForRestore = false
        try {
            manager.reconnectIfEnabled()
            val unavailableA = awaitValue("access-unavailable service API") { manager.liveConnection() }
            val unavailable = client.folderSyncStatus(unavailableA.baseUrl, unavailableA.token)
            assertEquals(FolderSyncAvailability.NEEDS_ATTENTION, unavailable.availability)
            assertEquals(FolderSyncIssue.FOLDER_ACCESS, unavailable.issue)
            assertEquals(FolderSyncLifecycle.STOPPED, unavailable.lifecycle)
            assertEquals(FolderHealthFreshness.NEVER_OBSERVED, unavailable.healthFreshness)
            assertEquals(PeerConnectionFreshness.NEVER_OBSERVED, unavailable.connectionFreshness)
            assertTrue(unavailable.folders.isEmpty())
            val unavailableShare = unavailable.shares.single { it.offerId == offerId }
            assertEquals(receipt.peerId, unavailableShare.peerId)
            assertEquals(FolderSharePhase.READY, unavailableShare.phase)
            assertEquals(PeerConnectionState.UNKNOWN, unavailableShare.peerConnection)
            assertTrue(unavailable.peers.any { it.peerId == receipt.peerId })

            stopServiceAndAwaitWorkers(unavailableA, remainingWorkers = 0)
            manager.reconnectIfEnabled()
            val deniedRestart = awaitValue("denied cold restart API") { manager.liveConnection() }
            assertEquals(
                FolderSyncIssue.FOLDER_ACCESS,
                client.folderSyncStatus(deniedRestart.baseUrl, deniedRestart.token).issue,
            )
            assertHelperCountsUnchangedFor(HelperCounts(0, 0), NEGATIVE_WINDOW_MILLIS)

            compose.setContent {
                CovalentTheme { if (showScreen.value) FolderSyncScreen(manager) }
            }
            clickScreenText(context.getString(R.string.action_refresh_backups))
            visibleScreenText(FOLDER_LABEL).assertIsDisplayed()
            visibleScreenText(context.getString(R.string.folder_sync_restore_access_detail)).assertIsDisplayed()
            clickScreenText(context.getString(R.string.folder_sync_remove))
            compose.onNodeWithText(context.getString(R.string.folder_sync_keep_sharing)).performClick()
            assertTrue(
                client.folderSyncStatus(deniedRestart.baseUrl, deniedRestart.token).shares
                    .any { it.offerId == offerId && it.phase != FolderSharePhase.REMOVED },
            )
            clickScreenText(context.getString(R.string.folder_sync_remove))
            compose.onNode(
                hasText(context.getString(R.string.folder_sync_remove)) and hasAnyAncestor(isDialog()),
            ).performClick()
            await("native offline removal recorded") {
                client.folderSyncStatus(deniedRestart.baseUrl, deniedRestart.token).shares
                    .single { it.offerId == offerId }.phase == FolderSharePhase.REMOVED
            }
            persistReceipt(receipt.copy(stage = RECEIPT_REMOVED))
            instrumentation.runOnMainSync { showScreen.value = false }
            preserveForRestore = true
        } finally {
            if (!preserveForRestore) {
                runCatching { instrumentation.runOnMainSync { showScreen.value = false } }
                context.stopService(Intent(context, NodeProviderService::class.java))
            }
        }
    }

    private fun runRestoredPhase(manager: EmbeddedNodeManager, runId: String) {
        val receipt = readReceipt(runId, RECEIPT_REMOVED)
        offerId = receipt.offerId
        installedEngine = FolderSyncInstrumentationBridge.isolatedPackage(context).engine
        fixtureRoot = fixtureDirectory(runId)
        secondData = secondDataDirectory(runId)
        await("host-restored all-files access") { FolderSyncSpecialAccess.granted() }
        try {
            assertHelperCountsUnchangedFor(HelperCounts(0, 0), NEGATIVE_WINDOW_MILLIS)
            val rootA = File(checkNotNull(fixtureRoot), "first")
            val rootB = File(checkNotNull(fixtureRoot), "second")
            assertArrayEquals(PAUSED_CONTENT, readBounded(File(rootA, "forward.txt")))
            assertArrayEquals(PAUSED_CONTENT, readBounded(File(rootB, "forward.txt")))
            assertArrayEquals(
                AFTER_ADDRESS_FORWARD_CONTENT,
                readBounded(File(rootA, "after-address-forward.txt")),
            )
            assertArrayEquals(
                AFTER_ADDRESS_FORWARD_CONTENT,
                readBounded(File(rootB, "after-address-forward.txt")),
            )
            assertArrayEquals(
                AFTER_ADDRESS_REVERSE_CONTENT,
                readBounded(File(rootA, "after-address-reverse.txt")),
            )
            assertArrayEquals(
                AFTER_ADDRESS_REVERSE_CONTENT,
                readBounded(File(rootB, "after-address-reverse.txt")),
            )
            assertArrayEquals(
                AFTER_ADDRESS_COLD_FORWARD_CONTENT,
                readBounded(File(rootA, "after-address-cold-forward.txt")),
            )
            assertArrayEquals(
                AFTER_ADDRESS_COLD_FORWARD_CONTENT,
                readBounded(File(rootB, "after-address-cold-forward.txt")),
            )
            assertArrayEquals(AFTER_RESTART_CONTENT, readBounded(File(rootA, "after-restart.txt")))
            assertArrayEquals(AFTER_RESTART_CONTENT, readBounded(File(rootB, "after-restart.txt")))
            manager.reconnectIfEnabled()
            val restored = awaitValue("restored local node API") { manager.liveConnection() }
            await("removed share remains durable after access restoration") {
                client.folderSyncStatus(restored.baseUrl, restored.token).shares
                    .single { it.offerId == receipt.offerId }.phase == FolderSharePhase.REMOVED
            }
            assertHelperCountsUnchangedFor(HelperCounts(0, 0), NEGATIVE_WINDOW_MILLIS)
        } finally {
            cleanup(manager, runId)
        }
        assertTrue(readAnyReceipt() == null)
        assertFalse(fixtureDirectory(runId).exists())
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
        } catch (timeout: Exception) {
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

    private fun clickScreenTag(value: String) {
        val target = compose.onNodeWithTag(value)
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { target.assertIsDisplayed().assertIsEnabled(); true }.getOrDefault(false)
        }
        target.performClick()
    }

    private fun startSecondNode(
        packageValue: PackagedSyncEnginePackage.Verified,
        peerListenerPort: Int = 0,
        folderSyncListenerPort: Int? = null,
    ): NodeConnection {
        val runId = requireRunId(InstrumentationRegistry.getArguments().getString(JOURNEY_RUN_ID_ARGUMENT))
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
            val peers = client.folderSyncStatus(connectionA.baseUrl, connectionA.token).peers
            "apiPairingCount=${requests.size} " +
                "apiCompletedPairingCount=${requests.count { it.state == NetworkPairingState.COMPLETE }} " +
                "apiPeerCount=${peers.size} expectedPeerPresent=${peers.any { it.peerId == expectedPeerId }}"
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
        val host = address.hostAddress.substringBefore('%')
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

    private fun createExternalFixture(runId: String): Pair<File, File> {
        val shared = RawFolderAccess(context).roots().firstOrNull()?.absolutePath
            ?.let(::File)?.canonicalFile
            ?: error("The emulator exposes no shared-storage root.")
        val fixture = File(shared, "$FIXTURE_PREFIX$runId").absoluteFile
        check(fixture.parentFile == shared)
        val first = File(fixture, "first")
        val second = File(fixture, "second")
        check(fixture.mkdir())
        fixtureRoot = fixture
        check(first.mkdir() && second.mkdir())
        return first to second
    }

    private fun shell(command: String): String {
        val descriptor = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
            val bytes = input.readBounded(MAX_SHELL_OUTPUT_BYTES)
            bytes.toString(StandardCharsets.UTF_8).trim()
        }
    }

    private fun writeNew(file: File, value: ByteArray) {
        Files.write(file.toPath(), value, StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE)
    }

    private fun overwrite(file: File, value: ByteArray) {
        Files.write(file.toPath(), value, StandardOpenOption.TRUNCATE_EXISTING, StandardOpenOption.WRITE)
    }

    private fun readBounded(file: File): ByteArray = FileInputStream(file).use { input ->
        input.readBounded(MAX_TEST_FILE_BYTES)
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

    private fun awaitFile(label: String, file: File, expected: ByteArray) {
        await(label, TRANSFER_TIMEOUT_MILLIS) {
            runCatching { readBounded(file).contentEquals(expected) }.getOrDefault(false)
        }
    }

    private fun assertFileUnchangedFor(file: File, expected: ByteArray, durationMillis: Long) {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(durationMillis)
        while (System.nanoTime() < deadline) {
            assertArrayEquals("A paused folder exchanged an edit", expected, readBounded(file))
            Thread.sleep(POLL_MILLIS)
        }
    }

    private fun stopServiceAndAwaitWorkers(priorConnection: NodeConnection, remainingWorkers: Int) {
        context.stopService(Intent(context, NodeProviderService::class.java))
        await("service API stop") {
            runCatching { client.status(priorConnection.baseUrl) }.isFailure
        }
        awaitExactHelperCounts(workers = remainingWorkers, guardians = remainingWorkers)
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

    private fun assertHelperCountsUnchangedFor(expected: HelperCounts, durationMillis: Long) {
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(durationMillis)
        while (System.nanoTime() < deadline) {
            assertEquals("Folder access denial relaunched a worker", expected, exactHelperCounts())
            Thread.sleep(POLL_MILLIS)
        }
    }

    private fun requireRunId(value: String?): String {
        if (value == null || !RUN_ID.matches(value)) {
            throw AssertionError("The host runner did not provide a canonical folder journey run ID.")
        }
        return value
    }

    private fun requirePriorProcessId(value: String?): Int {
        val parsed = value?.toIntOrNull()
        if (parsed == null || parsed <= 1) {
            throw AssertionError("The host runner did not provide the process it observed before denial.")
        }
        return parsed
    }

    private fun fixtureDirectory(runId: String): File {
        val shared = RawFolderAccess(context).roots().firstOrNull()?.absolutePath
            ?.let(::File)?.canonicalFile
            ?: error("The emulator exposes no shared-storage root.")
        return File(shared, "$FIXTURE_PREFIX${requireRunId(runId)}").absoluteFile.also {
            check(it.parentFile == shared)
        }
    }

    private fun secondDataDirectory(runId: String): File =
        File(context.noBackupFilesDir.canonicalFile, "$SECOND_DATA_PREFIX${requireRunId(runId)}")
            .absoluteFile.also { check(it.parentFile == context.noBackupFilesDir.canonicalFile) }

    private fun readAnyReceipt(): JourneyReceipt? {
        val preferences = context.getSharedPreferences(JOURNEY_RECEIPT_PREFERENCES, Context.MODE_PRIVATE)
        val values = preferences.all
        if (values.isEmpty()) return null
        if (values.keys != RECEIPT_KEYS) {
            throw AssertionError("The private folder journey receipt has an unknown shape.")
        }
        val runId = requireRunId(values[RECEIPT_RUN_ID] as? String)
        val stage = values[RECEIPT_STAGE] as? String
        val offer = values[RECEIPT_OFFER_ID] as? String
        val peer = values[RECEIPT_PEER_ID] as? String
        val process = values[RECEIPT_SETUP_PID] as? Int
        if (
            stage == null || stage !in setOf(RECEIPT_SETUP, RECEIPT_REMOVED) ||
            offer == null || offer.isBlank() || offer.length > MAX_RECEIPT_VALUE_CHARS ||
            peer == null || peer.isBlank() || peer.length > MAX_RECEIPT_VALUE_CHARS ||
            process == null || process <= 1
        ) {
            throw AssertionError("The private folder journey receipt is malformed.")
        }
        return JourneyReceipt(runId, stage, offer, peer, process)
    }

    private fun readReceipt(runId: String, stage: String): JourneyReceipt {
        val receipt = readAnyReceipt()
            ?: throw AssertionError("The preceding folder journey phase left no durable receipt.")
        if (receipt.runId != runId || receipt.stage != stage) {
            throw AssertionError("The private folder journey receipt does not match this phase.")
        }
        return receipt
    }

    private fun persistReceipt(receipt: JourneyReceipt) {
        val preferences = context.getSharedPreferences(JOURNEY_RECEIPT_PREFERENCES, Context.MODE_PRIVATE)
        check(
            preferences.edit().clear()
                .putString(RECEIPT_RUN_ID, requireRunId(receipt.runId))
                .putString(RECEIPT_STAGE, receipt.stage)
                .putString(RECEIPT_OFFER_ID, receipt.offerId)
                .putString(RECEIPT_PEER_ID, receipt.peerId)
                .putInt(RECEIPT_SETUP_PID, receipt.setupProcessId)
                .commit(),
        )
        assertEquals(receipt, readReceipt(receipt.runId, receipt.stage))
    }

    private fun cleanupStaleFixture(manager: EmbeddedNodeManager) {
        val stale = readAnyReceipt() ?: return
        installedEngine = FolderSyncInstrumentationBridge.isolatedPackage(context).engine
        fixtureRoot = fixtureDirectory(stale.runId)
        secondData = secondDataDirectory(stale.runId)
        manager.reconnectIfEnabled()
        val connection = awaitValue("stale journey local node API") { manager.liveConnection() }
        val oldShare = client.folderSyncStatus(connection.baseUrl, connection.token).shares
            .singleOrNull { it.offerId == stale.offerId }
        if (oldShare != null && oldShare.phase != FolderSharePhase.REMOVED) {
            client.removeFolder(connection.baseUrl, connection.token, stale.offerId)
            await("stale journey share removal") {
                client.folderSyncStatus(connection.baseUrl, connection.token).shares
                    .singleOrNull { it.offerId == stale.offerId }
                    ?.phase == FolderSharePhase.REMOVED
            }
        }
        cleanup(manager, stale.runId)
        assertTrue(readAnyReceipt() == null)
        assertFalse(fixtureDirectory(stale.runId).exists())
        installedEngine = null
        fixtureRoot = null
        secondData = null
    }

    private fun cleanup(manager: EmbeddedNodeManager, runId: String) {
        val failures = mutableListOf<Throwable>()
        fun attempt(action: () -> Unit) {
            runCatching(action).exceptionOrNull()?.let(failures::add)
        }
        attempt {
            check(
                context.getSharedPreferences("covalent_embedded_provider", Context.MODE_PRIVATE).edit()
                    .putBoolean("folder_sync_requested", false)
                    .putBoolean("enabled", false)
                    .putBoolean("running", false)
                    .commit(),
            )
        }
        attempt { context.stopService(Intent(context, NodeProviderService::class.java)) }
        attempt {
            if (secondHandle > 0) {
                val stopped = CovalentNative.stop(secondHandle)
                if (!stopped.ok) fail("The isolated native node did not stop cleanly.")
                secondHandle = 0
            }
        }
        forgetSecondNodeSecrets()
        var helpersReaped = installedEngine == null
        attempt {
            if (installedEngine != null) {
                await("all exact packaged helpers reaped", STOP_TIMEOUT_MILLIS) {
                    exactHelperCounts() == HelperCounts(0, 0)
                }
            }
            helpersReaped = true
        }
        attempt {
            check(
                context.getSharedPreferences("covalent_folder_sync_grants", Context.MODE_PRIVATE)
                    .edit().clear().commit(),
            )
        }
        if (helpersReaped) {
            // The host grants access before setup/restored cleanup. Every path is
            // derived from its canonical UUID run ID; cleanup never accepts a
            // provider or caller-supplied path and never follows a symbolic link.
            if (!FolderSyncSpecialAccess.granted()) {
                failures += AssertionError(
                    "Folder access disappeared before the owned external fixture was removed.",
                )
            } else {
                attempt { deleteTreeWithoutFollowingLinks(fixtureRoot ?: fixtureDirectory(runId)) }
            }
            attempt { deleteTreeWithoutFollowingLinks(secondData ?: secondDataDirectory(runId)) }
            attempt {
                installedEngine?.runtimeDirectory?.let(::File)?.takeIf(File::exists)
                    ?.let(::deleteTreeWithoutFollowingLinks)
            }
        }
        attempt { assertFalse(manager.folderSyncAccessUnavailable()) }
        // Keep the receipt whenever reaping, deletion, or state cleanup failed.
        // A later owned setup can then locate and safely finish cleanup instead
        // of leaving an anonymous external fixture behind. The host may clear
        // app data only after this restored phase returns success.
        if (failures.isEmpty()) {
            attempt {
                check(
                    context.getSharedPreferences(JOURNEY_RECEIPT_PREFERENCES, Context.MODE_PRIVATE)
                        .edit().clear().commit(),
                )
            }
        }
        if (failures.isNotEmpty()) {
            val error = AssertionError(
                "The API-37 folder journey did not clean up safely.",
                failures.first(),
            )
            failures.drop(1).forEach(error::addSuppressed)
            throw error
        }
    }

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

    private data class JourneyReceipt(
        val runId: String,
        val stage: String,
        val offerId: String,
        val peerId: String,
        val setupProcessId: Int,
    )

    private companion object {
        const val FOLDER_LABEL = "API 37 folder journey"
        const val JOURNEY_PHASE_ARGUMENT = "covalentFolderJourneyPhase"
        const val JOURNEY_RUN_ID_ARGUMENT = "covalentFolderJourneyRunId"
        const val JOURNEY_PRIOR_PID_ARGUMENT = "covalentFolderJourneyPriorPid"
        const val PHASE_SETUP = "setup"
        const val PHASE_DENIED = "denied"
        const val PHASE_RESTORED = "restored"
        const val JOURNEY_RECEIPT_PREFERENCES = "covalent_api37_folder_journey"
        const val RECEIPT_RUN_ID = "run_id"
        const val RECEIPT_STAGE = "stage"
        const val RECEIPT_OFFER_ID = "offer_id"
        const val RECEIPT_PEER_ID = "peer_id"
        const val RECEIPT_SETUP_PID = "setup_pid"
        const val RECEIPT_SETUP = "setup-complete"
        const val RECEIPT_REMOVED = "removal-complete"
        const val FIXTURE_PREFIX = "CovalentApi37-"
        const val SECOND_DATA_PREFIX = "journey-"
        const val MAX_RECEIPT_VALUE_CHARS = 512
        const val ADDRESS_CLASS_PRIVATE_LAN = 0
        const val ADDRESS_CLASS_TAILNET = 1
        const val ADDRESS_CLASS_CONTAINER_BRIDGE = 2
        val RUN_ID = Regex("[0-9a-f]{32}")
        val PAUSED_CONTENT = "api37-edit-held-while-paused\n".toByteArray(StandardCharsets.UTF_8)
        val AFTER_ADDRESS_FORWARD_CONTENT =
            "api37-forward-after-address-refresh\n".toByteArray(StandardCharsets.UTF_8)
        val AFTER_ADDRESS_REVERSE_CONTENT =
            "api37-reverse-after-address-refresh\n".toByteArray(StandardCharsets.UTF_8)
        val AFTER_ADDRESS_COLD_FORWARD_CONTENT =
            "api37-forward-after-address-cold-restart\n".toByteArray(StandardCharsets.UTF_8)
        val AFTER_RESTART_CONTENT = "api37-reverse-after-service-restart\n".toByteArray(StandardCharsets.UTF_8)
        val RECEIPT_KEYS = setOf(
            RECEIPT_RUN_ID,
            RECEIPT_STAGE,
            RECEIPT_OFFER_ID,
            RECEIPT_PEER_ID,
            RECEIPT_SETUP_PID,
        )
        const val POLL_MILLIS = 250L
        const val DEFAULT_TIMEOUT_MILLIS = 90_000L
        const val MAX_DIAGNOSTIC_NODES = 2
        const val TRANSFER_TIMEOUT_MILLIS = 120_000L
        const val STOP_TIMEOUT_MILLIS = 30_000L
        const val NEGATIVE_WINDOW_MILLIS = 7_000L
        const val PROCESS_SCAN_SECONDS = 5L
        const val MAX_PROC_ENTRIES = 4_096
        const val MAX_CLEANUP_ENTRIES = 100_000
        const val MAX_CLEANUP_DEPTH = 64
        const val MAX_TEST_FILE_BYTES = 64 * 1_024
        const val MAX_SHELL_OUTPUT_BYTES = 16 * 1_024
    }
}
