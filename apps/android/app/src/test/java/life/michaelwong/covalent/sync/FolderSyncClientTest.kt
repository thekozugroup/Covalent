package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncIssue
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.PeerConnectionFreshness
import life.michaelwong.covalent.model.PeerConnectionState
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONObject
import org.json.JSONArray
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderSyncClientTest {
    @Test
    fun authenticatedFolderRoutesUseTheBoundedV1Contract() {
        val server = MockWebServer()
        server.enqueue(MockResponse().setBody(STATUS))
        repeat(7) { server.enqueue(MockResponse().setBody(MUTATION)) }
        server.start()
        try {
            val base = server.url("/").toString().removeSuffix("/")
            val client = CovalentNodeClient()
            val status = client.folderSyncStatus(base, "token")
            assertEquals("Phone", status.peers.single().displayName)
            assertEquals(FolderSharePhase.OFFERED, status.shares.single().phase)
            assertFalse(status.shares.single().expired)
            assertEquals(PeerConnectionFreshness.FRESH, status.connectionFreshness)
            assertEquals(PeerConnectionState.DISCONNECTED, status.shares.single().peerConnection)

            client.offerFolder(base, "token", PEER, UUID.fromString(FOLDER), "Photos", "/storage/emulated/0/Photos")
            client.acceptFolder(base, "token", OFFER, "/storage/emulated/0/Shared")
            client.pauseFolder(base, "token", OFFER, true)
            client.repairFolder(base, "token", OFFER, "/storage/emulated/0/Repaired")
            client.removeFolder(base, "token", OFFER)
            client.retryFolderSync(base, "token")
            client.renewFolder(base, "token", OFFER)

            assertEquals("/api/v1/sync/status", server.takeRequest().path)
            assertEquals("/api/v1/sync/folders", server.takeRequest().path)
            assertEquals("/api/v1/sync/accept", server.takeRequest().path)
            assertEquals("/api/v1/sync/pause", server.takeRequest().path)
            val repair = server.takeRequest()
            assertEquals("/api/v1/sync/repair", repair.path)
            assertEquals("POST", repair.method)
            assertEquals("Bearer token", repair.getHeader("Authorization"))
            val repairBody = JSONObject(repair.body.readUtf8())
            assertEquals(setOf("offerId", "selectedRoot"), repairBody.keys().asSequence().toSet())
            assertEquals(OFFER, repairBody.getString("offerId"))
            assertEquals("/storage/emulated/0/Repaired", repairBody.getString("selectedRoot"))
            assertEquals("/api/v1/sync/remove", server.takeRequest().path)
            assertEquals("/api/v1/sync/retry", server.takeRequest().path)
            val renewal = server.takeRequest()
            assertEquals("/api/v1/sync/renew", renewal.path)
            assertEquals("POST", renewal.method)
            assertEquals("Bearer token", renewal.getHeader("Authorization"))
            val renewalBody = JSONObject(renewal.body.readUtf8())
            assertEquals(setOf("offerId"), renewalBody.keys().asSequence().toSet())
            assertEquals(OFFER, renewalBody.getString("offerId"))
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun statusRejectsForeignPeerReferencesAndUnknownPhases() {
        val invalid = STATUS.replace(PEER, "44444444-4444-4444-8444-444444444444", ignoreCase = false)
            .replaceFirst("44444444-4444-4444-8444-444444444444", PEER)
            .replace("\"offered\"", "\"future\"")
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(invalid))
            start()
        }
        try {
            val result = runCatching {
                CovalentNodeClient().folderSyncStatus(server.url("/").toString().removeSuffix("/"), "token")
            }
            assertTrue(result.isFailure)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun initialScanIsASeparateSafeLifecycle() {
        val scanning = STATUS
            .replace("\"lifecycle\":\"stopped\"", "\"lifecycle\":\"initialScanning\"")
            .replace("\"issue\":null", "\"issue\":\"initialScan\"")
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(scanning))
            start()
        }
        try {
            val status = CovalentNodeClient().folderSyncStatus(
                server.url("/").toString().removeSuffix("/"),
                "token",
            )
            assertEquals(FolderSyncLifecycle.INITIAL_SCANNING, status.lifecycle)
            assertEquals(FolderSyncIssue.INITIAL_SCAN, status.issue)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun offlineFolderAccessStatusRetainsRedactedSharesWithoutInventingHealth() {
        val offline = STATUS
            .replace("\"availability\":\"available\"", "\"availability\":\"needsAttention\"")
            .replace("\"issue\":null", "\"issue\":\"folderAccess\"")
            .replace("\"connectionFreshness\":\"fresh\"", "\"connectionFreshness\":\"neverObserved\"")
            .replace("\"peerConnection\":\"disconnected\"", "\"peerConnection\":\"unknown\"")
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(offline))
            start()
        }
        try {
            val status = CovalentNodeClient().folderSyncStatus(
                server.url("/").toString().removeSuffix("/"),
                "token",
            )
            assertEquals(FolderSyncLifecycle.STOPPED, status.lifecycle)
            assertEquals(FolderSyncIssue.FOLDER_ACCESS, status.issue)
            assertEquals(OFFER, status.shares.single().offerId)
            assertEquals(PeerConnectionFreshness.NEVER_OBSERVED, status.connectionFreshness)
            assertEquals(PeerConnectionState.UNKNOWN, status.shares.single().peerConnection)
            assertTrue(status.folders.isEmpty())
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun olderStatusDefaultsReachabilityToUnknownButMalformedValuesFail() {
        val legacy = STATUS
            .replace(",\"connectionFreshness\":\"fresh\"", "")
            .replace(",\"peerConnection\":\"disconnected\"", "")
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(legacy))
            enqueue(MockResponse().setBody(STATUS.replace("\"disconnected\"", "\"invented\"")))
            start()
        }
        try {
            val base = server.url("/").toString().removeSuffix("/")
            val old = CovalentNodeClient().folderSyncStatus(base, "token")
            assertEquals(PeerConnectionFreshness.NEVER_OBSERVED, old.connectionFreshness)
            assertEquals(PeerConnectionState.UNKNOWN, old.shares.single().peerConnection)
            assertTrue(runCatching { CovalentNodeClient().folderSyncStatus(base, "token") }.isFailure)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun replacementIdsAreBoundedUniqueAndCannotNameTheCurrentInvitation() {
        val oldId = "55555555-5555-4555-8555-555555555555"
        val variants = listOf(
            JSONArray().put(oldId), JSONObject.NULL, JSONArray().put(OFFER),
            JSONArray().put(oldId).put(oldId), JSONArray(List(129) { oldId }),
            JSONArray().put("00000000-0000-0000-0000-000000000000"),
        )
        val server = MockWebServer()
        variants.forEach { value ->
            val json = JSONObject(STATUS)
            json.getJSONArray("shares").getJSONObject(0).put("supersededOfferIds", value)
            server.enqueue(MockResponse().setBody(json.toString()))
        }
        server.start()
        try {
            val base = server.url("/").toString().removeSuffix("/")
            val client = CovalentNodeClient()
            assertEquals(listOf(oldId), client.folderSyncStatus(base, "token").shares.single().supersededOfferIds)
            repeat(variants.size - 1) {
                assertTrue(runCatching { client.folderSyncStatus(base, "token") }.isFailure)
            }
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun removalStatusPreservesRetiredIdsAndRejectsMalformedPendingFlags() {
        val oldId = "55555555-5555-4555-8555-555555555555"
        val server = MockWebServer()
        val variants = listOf(true, JSONObject.NULL, "true", 1)
        variants.forEach { value ->
            val json = JSONObject(STATUS)
            json.getJSONArray("shares").getJSONObject(0)
                .put("phase", "removed").put("supersededOfferIds", JSONArray().put(oldId))
                .put("remoteRemovalPending", value)
            server.enqueue(MockResponse().setBody(json.toString()))
        }
        val inconsistent = JSONObject(STATUS)
        inconsistent.getJSONArray("shares").getJSONObject(0).put("remoteRemovalPending", true)
        server.enqueue(MockResponse().setBody(inconsistent.toString()))
        server.start()
        try {
            val base = server.url("/").toString().removeSuffix("/")
            val client = CovalentNodeClient()
            val removed = client.folderSyncStatus(base, "token").shares.single()
            assertTrue(removed.remoteRemovalPending)
            assertEquals(listOf(oldId), removed.supersededOfferIds)
            repeat(variants.size) {
                assertTrue(runCatching { client.folderSyncStatus(base, "token") }.isFailure)
            }
        } finally {
            server.shutdown()
        }
    }

    private companion object {
        const val PEER = "22222222-2222-4222-8222-222222222222"
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        const val OFFER = "33333333-3333-4333-8333-333333333333"
        const val STATUS = """{
          "schemaVersion":1,"availability":"available","lifecycle":"stopped","issue":null,
          "healthFreshness":"neverObserved","connectionFreshness":"fresh",
          "peers":[{"peerId":"$PEER","displayName":"Phone"}],
          "shares":[{"offerId":"$OFFER","folderId":"$FOLDER","label":"Photos","peerId":"$PEER","incoming":true,"phase":"offered","expiresAtUnixMs":4102444800000,"expired":false,"peerConnection":"disconnected"}],
          "folders":[]
        }"""
        const val MUTATION = """{"schemaVersion":1,"offerId":"$OFFER","lifecycle":"stopped","issue":null}"""
    }
}
