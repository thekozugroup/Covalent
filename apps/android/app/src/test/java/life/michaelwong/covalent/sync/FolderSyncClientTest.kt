package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncIssue
import life.michaelwong.covalent.model.FolderSyncLifecycle
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderSyncClientTest {
    @Test
    fun authenticatedFolderRoutesUseTheBoundedV1Contract() {
        val server = MockWebServer()
        server.enqueue(MockResponse().setBody(STATUS))
        repeat(5) { server.enqueue(MockResponse().setBody(MUTATION)) }
        server.start()
        try {
            val base = server.url("/").toString().removeSuffix("/")
            val client = CovalentNodeClient()
            val status = client.folderSyncStatus(base, "token")
            assertEquals("Phone", status.peers.single().displayName)
            assertEquals(FolderSharePhase.OFFERED, status.shares.single().phase)
            assertFalse(status.shares.single().expired)

            client.offerFolder(base, "token", PEER, UUID.fromString(FOLDER), "Photos", "/storage/emulated/0/Photos")
            client.acceptFolder(base, "token", OFFER, "/storage/emulated/0/Shared")
            client.pauseFolder(base, "token", OFFER, true)
            client.removeFolder(base, "token", OFFER)
            client.retryFolderSync(base, "token")

            assertEquals("/api/v1/sync/status", server.takeRequest().path)
            assertEquals("/api/v1/sync/folders", server.takeRequest().path)
            assertEquals("/api/v1/sync/accept", server.takeRequest().path)
            assertEquals("/api/v1/sync/pause", server.takeRequest().path)
            assertEquals("/api/v1/sync/remove", server.takeRequest().path)
            assertEquals("/api/v1/sync/retry", server.takeRequest().path)
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

    private companion object {
        const val PEER = "22222222-2222-4222-8222-222222222222"
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        const val OFFER = "33333333-3333-4333-8333-333333333333"
        const val STATUS = """{
          "schemaVersion":1,"availability":"available","lifecycle":"stopped","issue":null,
          "healthFreshness":"neverObserved",
          "peers":[{"peerId":"$PEER","displayName":"Phone"}],
          "shares":[{"offerId":"$OFFER","folderId":"$FOLDER","label":"Photos","peerId":"$PEER","incoming":true,"phase":"offered","expiresAtUnixMs":4102444800000,"expired":false}],
          "folders":[]
        }"""
        const val MUTATION = """{"schemaVersion":1,"offerId":"$OFFER","lifecycle":"stopped","issue":null}"""
    }
}
