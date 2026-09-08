package life.michaelwong.covalent.node

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
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
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performTextInput
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.io.InputStream
import java.nio.charset.StandardCharsets
import java.nio.file.DirectoryStream
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.security.SecureRandom
import java.util.Base64
import java.util.UUID
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

/** Hosted API-37 proof of the real Android service, JNI, guardian, and packaged workers. */
class FolderSyncJourneyInstrumentedTest {
    @get:Rule
    val compose = createComposeRule()
    private val showScreen = mutableStateOf(true)
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val client = CovalentNodeClient()
    private var secondHandle = 0L
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

        val manager = EmbeddedNodeManager(context)

        try {
            setFolderAccess("default")
            grantLocalNetwork()
            await("default all-files access state") { !FolderSyncSpecialAccess.granted() }
            setFolderAccess("allow")
            await("all-files access grant") { FolderSyncSpecialAccess.granted() }
            assertFalse("The hosted app fixture must start without folder-sync demand", manager.folderSyncRequested())
            val packageValue = FolderSyncInstrumentationBridge.isolatedPackage(context)
            installedEngine = packageValue.engine
            val roots = createExternalFixture()
            val rootA = RawFolderAccess(context).select(roots.first.path)
            val rootB = RawFolderAccess(context).select(roots.second.path)
            assertTrue(manager.enableFolderSyncHost())
            val connectionA = awaitValue("service node API") { manager.liveConnection() }
            val identityA = client.transportIdentity(connectionA.baseUrl, connectionA.token)
            val connectionB = startSecondNode(packageValue)
            val identityB = client.transportIdentity(connectionB.baseUrl, connectionB.token)
            assertTrue(identityA.deviceId != identityB.deviceId)

            val forward = "api37-forward-before-offer\n".toByteArray(StandardCharsets.UTF_8)
            writeNew(File(rootA, "forward.txt"), forward)
            compose.setContent {
                CovalentTheme { if (showScreen.value) FolderSyncScreen(manager) }
            }
            pairUsingPhoneUi(connectionA, connectionB, identityB.peerPort)
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
            val paused = "api37-edit-held-while-paused\n".toByteArray(StandardCharsets.UTF_8)
            overwrite(File(rootA, "forward.txt"), paused)
            assertFileUnchangedFor(File(rootB, "forward.txt"), forward, NEGATIVE_WINDOW_MILLIS)
            clickScreenText(context.getString(R.string.action_resume))
            awaitFile("resumed transfer", File(rootB, "forward.txt"), paused)
            awaitExactHelperCounts(workers = 2, guardians = 2)

            stopServiceAndAwaitWorkers(checkNotNull(manager.localConnectionForFolderSync()), remainingWorkers = 1)
            manager.reconnectIfEnabled()
            val restartedA = awaitValue("cold restarted service node API") { manager.liveConnection() }
            val restartedIdentity = client.transportIdentity(restartedA.baseUrl, restartedA.token)
            assertEquals(identityA.deviceId, restartedIdentity.deviceId)
            assertEquals(identityA.peerPort, restartedIdentity.peerPort)
            assertEquals(identityA.certificateFingerprint, restartedIdentity.certificateFingerprint)
            assertEquals(identityA.certificateDer, restartedIdentity.certificateDer)
            awaitExactHelperCounts(workers = 2, guardians = 2)
            val afterRestart = "api37-reverse-after-service-restart\n".toByteArray(StandardCharsets.UTF_8)
            writeNew(File(rootB, "after-restart.txt"), afterRestart)
            awaitFile("transfer after cold service restart", File(rootA, "after-restart.txt"), afterRestart)

            // Both fixture nodes share this app UID. Stop the isolated peer
            // before revoking a UID-wide permission so its expected access
            // failure cannot obscure the production service's reaping proof.
            assertTrue(CovalentNative.stop(secondHandle).ok)
            secondHandle = 0
            awaitExactHelperCounts(workers = 1, guardians = 1)
            setFolderAccess("deny")
            await("special-access loss") { !FolderSyncSpecialAccess.granted() }
            awaitExactHelperCounts(workers = 0, guardians = 0)
            val unavailableA = awaitValue("access-unavailable service API") { manager.liveConnection() }
            val unavailable = client.folderSyncStatus(unavailableA.baseUrl, unavailableA.token)
            assertEquals(FolderSyncAvailability.NEEDS_ATTENTION, unavailable.availability)
            assertEquals(FolderSyncIssue.FOLDER_ACCESS, unavailable.issue)
            assertEquals(FolderSyncLifecycle.STOPPED, unavailable.lifecycle)
            assertEquals(FolderHealthFreshness.NEVER_OBSERVED, unavailable.healthFreshness)
            assertEquals(PeerConnectionFreshness.NEVER_OBSERVED, unavailable.connectionFreshness)
            assertTrue(unavailable.folders.isEmpty())
            val unavailableShare = unavailable.shares.single { it.offerId == offerId }
            assertEquals(identityB.deviceId, unavailableShare.peerId)
            assertEquals(FolderSharePhase.READY, unavailableShare.phase)
            assertEquals(PeerConnectionState.UNKNOWN, unavailableShare.peerConnection)
            assertTrue(unavailable.peers.any { it.peerId == identityB.deviceId })

            stopServiceAndAwaitWorkers(unavailableA, remainingWorkers = 0)
            manager.reconnectIfEnabled()
            val deniedRestart = awaitValue("denied cold restart API") { manager.liveConnection() }
            assertEquals(
                FolderSyncIssue.FOLDER_ACCESS,
                client.folderSyncStatus(deniedRestart.baseUrl, deniedRestart.token).issue,
            )
            assertHelperCountsUnchangedFor(HelperCounts(0, 0), NEGATIVE_WINDOW_MILLIS)

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
            // Only restore this disposable emulator permission after the
            // durable removal. Regaining access must not revive sharing.
            setFolderAccess("allow")
            await("fixture read access restored") { FolderSyncSpecialAccess.granted() }
            assertHelperCountsUnchangedFor(HelperCounts(0, 0), NEGATIVE_WINDOW_MILLIS)
            assertArrayEquals(paused, readBounded(File(rootA, "forward.txt")))
            assertArrayEquals(paused, readBounded(File(rootB, "forward.txt")))
            assertArrayEquals(afterRestart, readBounded(File(rootA, "after-restart.txt")))
            assertArrayEquals(afterRestart, readBounded(File(rootB, "after-restart.txt")))
            offerId = null
        } finally {
            try {
                instrumentation.runOnMainSync { showScreen.value = false }
            } finally {
                cleanup(manager)
            }
        }
    }

    private fun visibleScreenText(value: String): SemanticsNodeInteraction {
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching {
                compose.onNodeWithTag("folder-sync-list").performScrollToNode(hasText(value))
                compose.onNodeWithText(value).assertIsDisplayed()
                true
            }.getOrDefault(false)
        }
        return compose.onNodeWithText(value)
    }

    private fun clickScreenText(value: String) {
        val target = visibleScreenText(value)
        compose.waitUntil(timeoutMillis = DEFAULT_TIMEOUT_MILLIS) {
            runCatching { target.assertIsEnabled(); true }.getOrDefault(false)
        }
        target.performClick()
    }

    private fun startSecondNode(packageValue: PackagedSyncEnginePackage.Verified): NodeConnection {
        val data = File(context.noBackupFilesDir, "journey-${UUID.randomUUID()}").absoluteFile
        check(data.parentFile == context.noBackupFilesDir.canonicalFile)
        check(data.mkdir())
        secondData = data
        val random = SecureRandom()
        val tokenRandom = ByteArray(32).also(random::nextBytes)
        val token = Base64.getUrlEncoder().withoutPadding().encodeToString(tokenRandom)
        tokenRandom.fill(0)
        val response = CovalentNative.start(
            dataDirectory = data.path,
            deviceName = "API 37 isolated peer",
            lanDiscoveryEnabled = false,
            apiToken = token.toByteArray(StandardCharsets.US_ASCII),
            keyEncryptionKey = ByteArray(32).also(random::nextBytes),
            keyVersion = 1,
            maximumTotalBytes = 512L * 1024L * 1024L,
            freeSpaceReserveBytes = 0,
            keyProtectionLevel = KeyProtectionLevel.SOFTWARE,
            syncEngine = packageValue,
            backupProviderEnabled = false,
            folderSyncListenerPort = FolderSyncInstrumentationBridge.reserveEphemeralListenerPort(),
            peerListenerPort = 0,
        )
        assertTrue("The isolated JNI node must start", response.ok)
        secondHandle = checkNotNull(response.handle)
        return NodeConnection(checkNotNull(response.apiBaseUrl), token)
    }

    private fun pairUsingPhoneUi(
        connectionA: NodeConnection,
        connectionB: NodeConnection,
        peerPortB: Int,
    ) {
        compose.onNodeWithTag("folder-sync-list")
            .performScrollToNode(hasTestTag("folder-pair.address"))
        compose.onNodeWithTag("folder-pair.address").performTextInput("127.0.0.1:$peerPortB")
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
        await("initiator observes completed pairing") {
            client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
                .singleOrNull { it.pairingId == incoming.pairingId }
                ?.state == NetworkPairingState.COMPLETE
        }
        visibleScreenText(context.getString(R.string.folder_sync_pair_complete)).assertIsDisplayed()
        clickScreenText(context.getString(R.string.action_done))
        await("completed phone pairing request dismissed") {
            client.pendingNetworkPairings(connectionA.baseUrl, connectionA.token)
                .none { it.pairingId == incoming.pairingId }
        }
    }

    private fun EmbeddedNodeManager.liveConnection(): NodeConnection? {
        val value = localConnectionForFolderSync() ?: return null
        return value.takeIf { runCatching { client.status(it.baseUrl) }.isSuccess }
    }

    private fun createExternalFixture(): Pair<File, File> {
        val shared = RawFolderAccess(context).roots().firstOrNull()?.absolutePath
            ?.let(::File)?.canonicalFile
            ?: error("The emulator exposes no shared-storage root.")
        val fixture = File(shared, "CovalentApi37-${UUID.randomUUID()}").absoluteFile
        check(fixture.parentFile == shared)
        val first = File(fixture, "first")
        val second = File(fixture, "second")
        check(fixture.mkdir())
        fixtureRoot = fixture
        check(first.mkdir() && second.mkdir())
        return first to second
    }

    private fun grantLocalNetwork() {
        shell("pm grant ${context.packageName} ${Manifest.permission.ACCESS_LOCAL_NETWORK}")
        assertEquals(
            PackageManager.PERMISSION_GRANTED,
            context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK),
        )
    }

    private fun setFolderAccess(mode: String) {
        check(mode in setOf("allow", "deny", "default"))
        shell("appops set --uid ${context.packageName} MANAGE_EXTERNAL_STORAGE $mode")
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

    private fun cleanup(manager: EmbeddedNodeManager) {
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
            // Cleanup reads only test-owned external roots; access is restored
            // after exact helper reaping and reset even if deletion fails.
            attempt { setFolderAccess("allow") }
            attempt { fixtureRoot?.let(::deleteTreeWithoutFollowingLinks) }
            attempt { secondData?.let(::deleteTreeWithoutFollowingLinks) }
            attempt {
                installedEngine?.runtimeDirectory?.let(::File)?.takeIf(File::exists)
                    ?.let(::deleteTreeWithoutFollowingLinks)
            }
        }
        attempt { setFolderAccess("default") }
        attempt { assertFalse(manager.folderSyncAccessUnavailable()) }
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
        Files.walkFileTree(rootPath, object : java.nio.file.SimpleFileVisitor<Path>() {
            override fun visitFile(file: Path, attributes: java.nio.file.attribute.BasicFileAttributes) =
                Files.delete(file).let { java.nio.file.FileVisitResult.CONTINUE }

            override fun postVisitDirectory(directory: Path, error: java.io.IOException?) =
                if (error != null) throw error else Files.delete(directory).let {
                    java.nio.file.FileVisitResult.CONTINUE
                }
        })
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
        const val FOLDER_LABEL = "API 37 folder journey"
        const val POLL_MILLIS = 250L
        const val DEFAULT_TIMEOUT_MILLIS = 90_000L
        const val TRANSFER_TIMEOUT_MILLIS = 120_000L
        const val STOP_TIMEOUT_MILLIS = 30_000L
        const val NEGATIVE_WINDOW_MILLIS = 7_000L
        const val PROCESS_SCAN_SECONDS = 5L
        const val MAX_PROC_ENTRIES = 4_096
        const val MAX_TEST_FILE_BYTES = 64 * 1_024
        const val MAX_SHELL_OUTPUT_BYTES = 16 * 1_024
    }
}
