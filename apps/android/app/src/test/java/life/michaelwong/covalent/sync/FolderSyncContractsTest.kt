package life.michaelwong.covalent.sync

import life.michaelwong.covalent.model.FolderHealth
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.AndroidLinkConditions
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkCadence
import life.michaelwong.covalent.model.FolderLinkRunPhase
import life.michaelwong.covalent.model.FolderLinkRunSummary
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderLinkSettingsState
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderShareSummary
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncPeer
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.PeerConnectionFreshness
import life.michaelwong.covalent.model.PeerConnectionState
import life.michaelwong.covalent.model.summaryFor
import org.junit.Assert.assertEquals
import org.junit.Test

class FolderSyncContractsTest {
    @Test
    fun errorsAndScanningPrecedeReachabilityWhileDisconnectNamesWaitingState() {
        val share = share(PeerConnectionState.DISCONNECTED)
        assertEquals(FolderShareSummary.WAITING_FOR_PEER, status(share).summaryFor(share))
        assertEquals(
            FolderShareSummary.CHECKING,
            status(share, lifecycle = FolderSyncLifecycle.INITIAL_SCANNING).summaryFor(share),
        )
        val errored = status(share).copy(folders = listOf(health.copy(statusError = true)))
        assertEquals(FolderShareSummary.NEEDS_ATTENTION, errored.summaryFor(share))
    }

    @Test
    fun activeLinkRunPrecedesDisconnectedPeerUnlessAnErrorExists() {
        val disconnected = share(PeerConnectionState.DISCONNECTED)
        val preparing = disconnected.copy(linkRun = linkRun(FolderLinkRunPhase.PREPARING))
        val running = disconnected.copy(linkRun = linkRun(FolderLinkRunPhase.RUNNING))
        assertEquals(FolderShareSummary.CHECKING, status(preparing).summaryFor(preparing))
        assertEquals(FolderShareSummary.SYNCING, status(running).summaryFor(running))
        assertEquals(
            FolderShareSummary.WAITING_FOR_PEER,
            status(running).copy(availability = FolderSyncAvailability.NOT_PACKAGED).summaryFor(running),
        )
        val errored = status(running).copy(folders = listOf(health.copy(statusError = true)))
        assertEquals(FolderShareSummary.NEEDS_ATTENTION, errored.summaryFor(running))
    }

    @Test
    fun staleOrMissingObservationNeverClaimsConnected() {
        val share = share(PeerConnectionState.CONNECTED)
        val stale = status(share).copy(connectionFreshness = PeerConnectionFreshness.STALE)
        assertEquals(FolderShareSummary.CONNECTION_UNKNOWN, stale.summaryFor(share))
        assertEquals(FolderShareSummary.CONNECTED, status(share).summaryFor(share))
    }

    @Test
    fun removedShareNeverReportsAnOldConnectionOrFolderHealth() {
        val removed = share(PeerConnectionState.CONNECTED).copy(phase = FolderSharePhase.REMOVED)
        val oldHealth = status(removed, lifecycle = FolderSyncLifecycle.INITIAL_SCANNING)
        assertEquals(FolderShareSummary.REMOVED, oldHealth.summaryFor(removed))
        assertEquals(FolderShareSummary.REMOVAL_PENDING, oldHealth.summaryFor(removed.copy(remoteRemovalPending = true)))
    }

    @Test
    fun oneWayIdleNeedCountsDoNotOverrideActiveStateOrErrors() {
        val share = share(PeerConnectionState.CONNECTED).copy(linkPolicy = FolderLinkPolicy())
        val idleNeed = status(share).copy(folders = listOf(health.copy(remainingFiles = 2, remainingBytes = 40)))
        assertEquals(FolderShareSummary.CONNECTED, idleNeed.summaryFor(share))

        val syncing = idleNeed.copy(folders = listOf(health.copy(state = "syncing", remainingFiles = 2)))
        assertEquals(FolderShareSummary.SYNCING, syncing.summaryFor(share))

        val errored = idleNeed.copy(folders = listOf(health.copy(statusError = true, remainingFiles = 2)))
        assertEquals(FolderShareSummary.NEEDS_ATTENTION, errored.summaryFor(share))

        val legacy = share.copy(linkPolicy = null)
        assertEquals(FolderShareSummary.SYNCING, idleNeed.copy(shares = listOf(legacy)).summaryFor(legacy))
    }

    @Test
    fun healthyManualAndScheduledIdleAreReadyWhileContinuousStillRequiresRuntime() {
        fun idle(cadence: FolderLinkCadence) = share(PeerConnectionState.UNKNOWN).copy(
            linkPolicy = FolderLinkPolicy(),
            linkSettings = FolderLinkSettingsState(
                0, FolderLinkSettings(FolderLinkPolicy(), false, cadence),
                "00000000-0000-0000-0000-000000000000", PEER, true, null, null,
            ),
        )
        val manual = idle(FolderLinkCadence.Manual)
        val scheduled = idle(FolderLinkCadence.Scheduled(60))
        val continuous = idle(FolderLinkCadence.Continuous)
        assertEquals(FolderShareSummary.READY, status(manual, FolderSyncLifecycle.STOPPED).summaryFor(manual))
        assertEquals(FolderShareSummary.READY, status(scheduled, FolderSyncLifecycle.STOPPED).summaryFor(scheduled))
        assertEquals(FolderShareSummary.OFFLINE, status(continuous, FolderSyncLifecycle.STOPPED).summaryFor(continuous))
        val inaccessible = status(manual, FolderSyncLifecycle.STOPPED).copy(
            healthFreshness = FolderHealthFreshness.STALE,
            folders = listOf(health.copy(state = "error", accessUnavailable = true)),
        )
        assertEquals(FolderShareSummary.NEEDS_ATTENTION, inaccessible.summaryFor(manual))
        assertEquals(FolderShareSummary.READY, inaccessible.summaryFor(manual.copy(folderId = PEER)))
    }

    @Test
    fun sharedAndroidConditionsExplainAnIntentionalStopWithoutHidingErrorsOrPause() {
        val waiting = share(PeerConnectionState.DISCONNECTED).copy(
            waitingForConditions = true,
            linkPolicy = FolderLinkPolicy(),
            linkSettings = FolderLinkSettingsState(
                1, FolderLinkSettings(
                    FolderLinkPolicy(),
                    false,
                    FolderLinkCadence.Continuous,
                    AndroidLinkConditions(wifiOnly = true),
                ),
                "00000000-0000-0000-0000-000000000000", PEER, true, null, null,
            ),
        )
        assertEquals(
            FolderShareSummary.WAITING_FOR_CONDITIONS,
            status(waiting, FolderSyncLifecycle.STOPPED).summaryFor(waiting),
        )
        assertEquals(
            FolderShareSummary.NEEDS_ATTENTION,
            status(waiting, FolderSyncLifecycle.NEEDS_ATTENTION).summaryFor(waiting),
        )
        val paused = waiting.copy(phase = FolderSharePhase.PAUSED)
        assertEquals(FolderShareSummary.PAUSED, status(paused, FolderSyncLifecycle.STOPPED).summaryFor(paused))
    }

    private fun share(connection: PeerConnectionState) = FolderShare(
        offerId = "33333333-3333-4333-8333-333333333333",
        folderId = FOLDER,
        label = "Plans",
        peerId = PEER,
        incoming = false,
        phase = FolderSharePhase.READY,
        expiresAtUnixMs = null,
        expired = false,
        peerConnection = connection,
    )

    private fun status(
        share: FolderShare,
        lifecycle: FolderSyncLifecycle = FolderSyncLifecycle.RUNNING,
    ) = FolderSyncStatus(
        availability = FolderSyncAvailability.AVAILABLE,
        lifecycle = lifecycle,
        issue = null,
        healthFreshness = FolderHealthFreshness.FRESH,
        peers = listOf(FolderSyncPeer(PEER, "Kitchen server")),
        shares = listOf(share),
        folders = listOf(health),
        connectionFreshness = PeerConnectionFreshness.FRESH,
    )

    private fun linkRun(phase: FolderLinkRunPhase) = FolderLinkRunSummary(
        1, 1, 1, phase, 0, null, null, null, null, null, emptyList(),
    )

    private companion object {
        const val PEER = "22222222-2222-4222-8222-222222222222"
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        val health = FolderHealth(FOLDER, "idle", 0, 0, 0, 0, false, false)
    }
}
