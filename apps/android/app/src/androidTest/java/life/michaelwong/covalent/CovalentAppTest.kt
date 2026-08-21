package life.michaelwong.covalent

import android.app.job.JobInfo
import android.content.ComponentName
import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.compose.material3.MaterialTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsToggleable
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onAllNodesWithText
import androidx.lifecycle.SavedStateHandle
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.ui.CovalentApp
import life.michaelwong.covalent.ui.ConnectionHealth
import life.michaelwong.covalent.ui.CovalentViewModel
import life.michaelwong.covalent.ui.PairingRole
import life.michaelwong.covalent.ui.Screen
import life.michaelwong.covalent.ui.PrimaryActionToolbar
import life.michaelwong.covalent.ui.validateAndPersistSetup
import life.michaelwong.covalent.ui.theme.CovalentTheme
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.EnrolledTrust
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.PlatformTier
import life.michaelwong.covalent.model.RememberedBackup
import life.michaelwong.covalent.model.RestorePlanPage
import life.michaelwong.covalent.model.RestorePlanReference
import life.michaelwong.covalent.model.RestorePreviewEntry
import life.michaelwong.covalent.model.TransferKind
import life.michaelwong.covalent.model.TransferRecord
import life.michaelwong.covalent.model.TransferState
import life.michaelwong.covalent.work.TransferJobService
import life.michaelwong.covalent.work.TransferScheduler
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.json.JSONArray
import org.json.JSONObject

class CovalentAppTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun firstLaunchRequiresDirectNodeConnection() {
        val isolatedStore = isolatedStore("first_launch")
        val state = CovalentViewModel(SavedStateHandle())
        compose.setContent {
            CovalentTheme {
                CovalentApp(isolatedStore, state)
            }
        }
        compose.onNodeWithText("Connect your backup server").assertIsDisplayed()
        compose.onNodeWithText("Server access token").assertIsDisplayed()
    }

    @Test
    fun failedSetupValidationDoesNotPersistCredentials() {
        val store = isolatedStore("failed_setup")
        val result = runCatching {
            validateAndPersistSetup(
                CovalentNodeClient(),
                store,
                "Untrusted input",
                "not-a-url",
                "must-not-be-saved",
            )
        }

        assertTrue(result.isFailure)
        assertEquals("", store.baseUrl)
        assertEquals("", store.displayName)
        assertEquals("", store.token)
    }

    @Test
    fun floatingToolbarKeepsTheThreeTierOneActionsAccessible() {
        compose.setContent { CovalentTheme { PrimaryActionToolbar(enabled = true, onAction = {}) } }
        compose.onNodeWithText("Pair").assertIsDisplayed()
        compose.onNodeWithText("Backup").assertIsDisplayed()
        compose.onNodeWithText("Restore").assertIsDisplayed()
    }

    @Test
    fun largeTextToolbarUsesStableIconFirstAccessibleActions() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale = 1.5f)) {
                CovalentTheme {
                    PrimaryActionToolbar(enabled = true, compact = true, onAction = {})
                }
            }
        }
        compose.onNodeWithContentDescription("Pair").assertIsDisplayed()
        compose.onNodeWithContentDescription("Backup").assertIsDisplayed()
        compose.onNodeWithContentDescription("Restore").assertIsDisplayed()
    }

    @Test
    fun narrowWindowKeepsAllPrimaryActionsAccessible() {
        compose.setContent {
            CovalentTheme {
                Box(Modifier.width(320.dp)) {
                    PrimaryActionToolbar(enabled = true, onAction = {})
                }
            }
        }
        compose.onNodeWithContentDescription("Pair").assertIsDisplayed()
        compose.onNodeWithContentDescription("Backup").assertIsDisplayed()
        compose.onNodeWithContentDescription("Restore").assertIsDisplayed()
    }

    @Test
    fun lanDiscoveryIsOneLabeledToggleWithServerDerivedState() {
        val store = isolatedStore("lan_semantics")
        val state = readyState(store, Screen.SETTINGS)
        compose.setContent { CovalentTheme { CovalentApp(store, state) } }

        compose.onNodeWithText("Nearby discovery").assertIsDisplayed().assertIsToggleable()
    }

    @Test
    fun eachPartyReplicaRoleRowExposesOneToggleTarget() {
        val store = isolatedStore("replica_semantics")
        val state = readyState(store, Screen.PAIR).apply { pairingRole = PairingRole.RESPONDER }
        compose.setContent { CovalentTheme { CovalentApp(store, state) } }

        val replicaRows = compose.onAllNodesWithText("Store encrypted replica chunks")
        replicaRows[0].assertIsDisplayed().assertIsToggleable()
        replicaRows[1].assertIsDisplayed().assertIsToggleable()
    }

    @Test
    fun restoreUsesRememberedBackupInsteadOfEditableIdentifiers() {
        val store = isolatedStore("restore_picker").apply {
            replaceBackups(listOf(RememberedBackup(
                backupId = "11111111-1111-4111-8111-111111111111",
                name = "Backup Alpha",
                ownerDeviceId = "22222222-2222-4222-8222-222222222222",
                latestSnapshotId = "snapshot-one",
                latestCommittedAtUnixMs = 1,
                snapshotCount = 3,
                selectedProviderIds = emptySet(),
            )))
        }
        val state = readyState(store, Screen.RESTORE)
        compose.setContent { CovalentTheme { CovalentApp(store, state) } }

        compose.onNodeWithText("Backup Alpha").assertIsDisplayed().assertIsToggleable()
        compose.onNodeWithText("Backup ID").assertDoesNotExist()
        compose.onNodeWithText("Snapshot ID").assertDoesNotExist()
    }

    @Test
    fun restorePreviewShowsOneBoundedPathPageAndPaginationControl() {
        val backup = RememberedBackup(
            backupId = "11111111-1111-4111-8111-111111111111",
            name = "Backup Alpha",
            ownerDeviceId = "22222222-2222-4222-8222-222222222222",
            latestSnapshotId = "snapshot-one",
            latestCommittedAtUnixMs = 1,
            snapshotCount = 3,
            selectedProviderIds = emptySet(),
        )
        val store = isolatedStore("restore_page").apply { replaceBackups(listOf(backup)) }
        val state = readyState(store, Screen.RESTORE).apply {
            selectedRestoreBackupId = backup.backupId
            persistRestorePlan(RestorePlanPage(
                reference = RestorePlanReference(
                    planId = "0123456789abcdef0123456789abcdef",
                    planDigest = "abcdef0123456789abcdef0123456789",
                    backupId = backup.backupId,
                    snapshotId = "snapshot-one",
                    authorizedRoot = "/private/target",
                    manifestDigest = "manifest",
                    conflictPolicy = "fail",
                    jobId = "restore-job",
                    signerDeviceId = "node-one",
                    signature = "signature",
                    totalEntries = 101,
                ),
                entryOffset = 0,
                entries = listOf(RestorePreviewEntry("Documents/one.txt", "file", "create_file")),
                nextCursor = "1",
            ))
        }
        compose.setContent { CovalentTheme { CovalentApp(store, state) } }

        compose.onNodeWithText("Documents/one.txt").assertIsDisplayed()
        compose.onNodeWithText("Showing paths 1–1").assertIsDisplayed()
        compose.onNodeWithText("Show next paths").assertIsDisplayed()
    }

    @Test
    fun settingsCandidateRequiresVisiblePreviewAndSeparateConfirmation() {
        val store = isolatedStore("settings_preview")
        val state = readyState(store, Screen.SETTINGS).apply {
            setImportCandidate(
                JSONObject()
                    .put("schemaVersion", 1)
                    .put("deviceName", "After")
                    .put("lanDiscoveryEnabled", false)
                    .put("rememberedBackups", JSONArray()),
                JSONObject()
                    .put("schemaVersion", 1)
                    .put("deviceName", "Before")
                    .put("lanDiscoveryEnabled", true)
                    .put("rememberedBackups", JSONArray().put(JSONObject())),
            )
        }
        compose.setContent { CovalentTheme { CovalentApp(store, state) } }

        compose.onNodeWithText("Review settings changes").assertIsDisplayed()
        compose.onNodeWithText("This import removes remembered backups from your backup server.")
            .assertIsDisplayed()
        compose.onNodeWithText("Confirm import").assertIsDisplayed()
    }

    @Test
    fun persistedPausedTransferShowsResumeAndSurvivesStateReload() {
        val store = isolatedStore("transfer_lifecycle")
        store.saveTransfer(TransferRecord(
            jobId = "backup-persisted",
            label = "Documents",
            kind = TransferKind.BACKUP,
            state = TransferState.PAUSED,
            detail = "Paused for test",
            retryable = true,
        ))
        assertEquals(TransferState.PAUSED, store.transfer("backup-persisted")?.state)
        val state = readyState(store, Screen.HOME)
        compose.setContent { CovalentTheme { CovalentApp(store, state) } }

        compose.onNodeWithText("Documents").assertIsDisplayed()
        compose.onNodeWithText("Resume").assertIsDisplayed()
        compose.onNodeWithText("Cancel").assertIsDisplayed()
    }

    @Test
    fun retainedJobAcknowledgementSurvivesProcessStateReload() {
        val store = isolatedStore("retained_ack")
        store.savePendingAcknowledgement("restore-retained", "Restore complete")
        store.savePendingDiscard("backup-cancelled", "Cancellation confirmed")

        assertEquals(listOf("restore-retained"), store.pendingAcknowledgementJobIds())
        assertEquals(
            "Restore complete",
            store.acknowledgementCompletionDetail("restore-retained"),
        )
        assertEquals(listOf("backup-cancelled"), store.pendingDiscardJobIds())
        assertEquals("Cancellation confirmed", store.discardCompletionDetail("backup-cancelled"))
        store.removePendingAcknowledgement("restore-retained")
        store.removePendingDiscard("backup-cancelled")
        assertTrue(store.pendingAcknowledgementJobIds().isEmpty())
        assertTrue(store.pendingDiscardJobIds().isEmpty())
    }

    @Test
    fun packagedCaddyRequiresEnrolledTrustAndAuthenticatedAccess() {
        val arguments = InstrumentationRegistry.getArguments()
        val baseUrl = arguments.getString("covalentTlsBaseUrl") ?: return
        val token = checkNotNull(arguments.getString("covalentTlsToken"))
        val ca = checkNotNull(arguments.getString("covalentTlsCa"))
        val wrongCa = checkNotNull(arguments.getString("covalentTlsWrongCa"))
        val pin = checkNotNull(arguments.getString("covalentTlsPin"))
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        if (Build.VERSION.SDK_INT >= 37) {
            ParcelFileDescriptor.AutoCloseInputStream(
                instrumentation.uiAutomation.executeShellCommand(
                    "pm grant ${context.packageName} android.permission.ACCESS_LOCAL_NETWORK",
                ),
            ).use { it.readBytes() }
            assertEquals(
                PackageManager.PERMISSION_GRANTED,
                context.checkSelfPermission("android.permission.ACCESS_LOCAL_NETWORK"),
            )
        }

        assertTrue(runCatching { CovalentNodeClient().status(baseUrl) }.isFailure)
        assertTrue(
            runCatching {
                CovalentNodeClient { EnrolledTrust(caCertificateDerBase64 = wrongCa) }.status(baseUrl)
            }.isFailure,
        )

        val caClient = CovalentNodeClient { EnrolledTrust(caCertificateDerBase64 = ca) }
        assertEquals("Android TLS node", caClient.status(baseUrl).deviceName)
        assertTrue(caClient.backups(baseUrl, token).isEmpty())
        val authenticationError = runCatching { caClient.backups(baseUrl, "${token}invalid") }.exceptionOrNull()
        assertTrue(authenticationError is NodeApiException && authenticationError.statusCode == 401)

        val pinClient = CovalentNodeClient { EnrolledTrust(sha256Pin = pin) }
        assertEquals("Android TLS node", pinClient.status(baseUrl).deviceName)
        assertTrue(pinClient.backups(baseUrl, token).isEmpty())
    }

    @Test
    fun api34PlusTransferJobIsUserInitiatedPersistedAndNetworkBound() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) return
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val job = TransferScheduler.buildUserInitiatedJob(context, "backup-regression", 42_001)

        assertTrue(job.isUserInitiated)
        assertTrue(job.isPersisted)
        assertNotNull(job.requiredNetwork)
        assertEquals(ComponentName(context, TransferJobService::class.java), job.service)
        assertEquals("backup-regression", job.extras.getString("job_id"))
        assertEquals(JobInfo.PRIORITY_MAX, job.priority)
    }

    @Test
    fun darkAppearanceUsesARealDarkSurfaceDeterministically() {
        var background = Color.Unspecified
        compose.setContent {
            CovalentTheme(darkTheme = true, dynamicColor = false) {
                background = MaterialTheme.colorScheme.background
            }
        }
        compose.runOnIdle {
            assertTrue(background != Color.Unspecified)
            assertTrue(background.luminance() < 0.2f)
        }
    }


    private fun isolatedStore(suffix: String): SecureNodeStore {
        val base = InstrumentationRegistry.getInstrumentation().targetContext
        val isolatedContext = object : ContextWrapper(base) {
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                base.getSharedPreferences("instrumentation_${suffix}_$name", Context.MODE_PRIVATE)
        }
        isolatedContext.getSharedPreferences("covalent_node", Context.MODE_PRIVATE)
            .edit().clear().commit()
        return SecureNodeStore(isolatedContext)
    }

    private fun readyState(store: SecureNodeStore, screen: Screen): CovalentViewModel =
        CovalentViewModel(SavedStateHandle()).apply {
            initialize(store)
            this.screen = screen
            status = NodeStatus("Test node", 1u, false, PlatformTier.TIER_1, "ready")
            connectionHealth = ConnectionHealth.READY
        }
}
