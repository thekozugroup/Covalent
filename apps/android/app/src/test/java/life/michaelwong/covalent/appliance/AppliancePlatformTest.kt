package life.michaelwong.covalent.appliance

import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.NodeApiException
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class AppliancePlatformTest {
    @Test fun javaProjectionFixtureAndUnknownObservationsDecodeWithoutInventedCounts() {
        val result = parseAppliancePlatform(envelope(), PI.deviceId, PHONE.deviceId)
        assertEquals(7L, result.snapshot!!.sources.single().count)
        assertEquals(123456L, result.snapshot.spaceAfterReserve)
        assertTrue(result.snapshot.updates!!.automatic)
        val unavailable = envelope().put("platformAvailable", false).put("snapshot", JSONObject.NULL)
        assertNull(parseAppliancePlatform(unavailable, PI.deviceId, PHONE.deviceId).snapshot)
        val missing = envelope(); missing.getJSONObject("snapshot").put("storage", JSONObject.NULL).put("updates", JSONObject.NULL)
        val source = missing.getJSONObject("snapshot").getJSONArray("sources").getJSONObject(0)
        source.put("connected", false).put("preview", JSONObject().put("state", "unavailable").put("counts", JSONObject.NULL))
        val decoded = parseAppliancePlatform(missing, PI.deviceId, PHONE.deviceId).snapshot!!
        assertNull(decoded.spaceAfterReserve);assertNull(decoded.updates);assertNull(decoded.sources.single().count)
    }

    @Test fun clientUsesLocalReadOnlyRouteBearerAndBoundedNonredirectingResponses() {
        MockWebServer().use { server -> MockWebServer().use { other ->
            val client = CovalentNodeClient(); val url = server.url("/").toString().removeSuffix("/")
            server.enqueue(MockResponse().setBody(envelope().toString()))
            assertEquals(7L, client.appliancePlatform(url, TOKEN, PI.deviceId, PHONE.deviceId).snapshot!!.sources.single().count)
            val request = server.takeRequest()
            assertEquals("POST", request.method);assertEquals("/api/v1/appliance/platform", request.path)
            assertEquals("Bearer $TOKEN", request.getHeader("Authorization"));assertEquals("{\"peerId\":\"${PI.deviceId}\"}", request.body.readUtf8())
            server.enqueue(MockResponse().setResponseCode(302).setHeader("Location", other.url("/")))
            assertThrows(NodeApiException::class.java) { client.appliancePlatform(url, TOKEN, PI.deviceId, PHONE.deviceId) }
            assertEquals(0, other.requestCount)
            server.enqueue(MockResponse().setBody(" ".repeat(40_961)))
            assertThrows(Exception::class.java) { client.appliancePlatform(url, TOKEN, PI.deviceId, PHONE.deviceId) }
        } }
    }

    @Test fun malformedAndForgedPlatformResponsesNeverReachTheDashboard() {
        val changes = listOf<(JSONObject) -> Unit>(
            { it.put("schemaVersion", 2) }, { it.put("applianceId", PHONE.deviceId) }, { it.put("controllerId", PI.deviceId) },
            { it.put("bondGeneration", "00000000-0000-0000-0000-000000000000") },
            { it.put("platformSupported", false) }, { it.put("platformAvailable", false) }, { it.remove("snapshot") },
            { it.getJSONObject("snapshot").put("importApprovalAvailable", true) },
            { it.getJSONObject("snapshot").put("sourceCleanupAvailable", true) },
            { it.getJSONObject("snapshot").put("token", "private") },
            { it.getJSONObject("snapshot").put("sourcesTruncated", "false") },
            { it.getJSONObject("snapshot").put("volumeServiceAvailable", false) },
            { it.getJSONObject("snapshot").getJSONObject("storage").put("reserveBytes", 0) },
            { it.getJSONObject("snapshot").getJSONObject("storage").put("availableAfterReserveBytes", -1) },
            { it.getJSONObject("snapshot").getJSONObject("updates").put("configured", "true") },
            { source(it).put("sourceId", "raw-volume") }, { source(it).put("name", "\u202eprivate") },
            { source(it).put("destination", "/data/private") }, { source(it).put("kind", "disk") },
            { source(it).put("connected", false) }, { source(it).put("automatic", "false") },
            { source(it).getJSONObject("preview").put("state", "scanning") },
            { source(it).getJSONObject("preview").getJSONObject("counts").put("eligibleFiles", 100001) },
            { source(it).getJSONObject("preview").getJSONObject("counts").put("eligibleBytes", 1) },
            { source(it).getJSONObject("preview").getJSONObject("counts").put("eligibleBytes", "2048") },
            { source(it).getJSONObject("preview").getJSONObject("counts").put("eligibleFiles", 7.5) },
            { it.getJSONObject("snapshot").getJSONArray("sources").put(source(it)) },
        )
        changes.forEach { change -> assertThrows(Exception::class.java) {
            parseAppliancePlatform(envelope().also(change), PI.deviceId, PHONE.deviceId)
        } }
    }

    @Test fun changedBondOrSelectionDuringPlatformReadDiscardsEveryOldObservation() {
        val disk = Disk();val store = ApplianceSelectionStore(disk);store.replace(null, SELECTED)
        var platform: () -> AppliancePlatformObservation? = { parseAppliancePlatform(envelope(), PI.deviceId, PHONE.deviceId) }
        val api = object : ApplianceDashboardApi {
            override fun context() = ApplianceContext(PHONE, listOf(PI))
            override fun names() = mapOf(PI.deviceId to "Pi")
            override fun status(peerId: String, localId: String) = ApplianceObservation(GENERATION, ApplianceSyncState.RUNNING, 1, true)
            override fun platform(peerId: String, localId: String) = platform()
        }
        val controller = ApplianceDashboardController(store, api)
        assertNotNull(controller.refresh().platform!!.snapshot)
        platform = { parseAppliancePlatform(envelope().put("bondGeneration", "55555555-5555-4555-8555-555555555555"), PI.deviceId, PHONE.deviceId) }
        val changed = controller.refresh();assertEquals(ApplianceConnectionState.UNAVAILABLE, changed.connection)
        assertNull(changed.platform);assertNull(changed.observation)
        platform = { store.replace(SELECTED.copy(confirmedGeneration = GENERATION), SELECTED.copy(name = "New choice"));parseAppliancePlatform(envelope(), PI.deviceId, PHONE.deviceId) }
        val delayed = controller.refresh();assertEquals(ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE, delayed.connection)
        assertNull(delayed.platform);assertNull(delayed.observation);assertEquals("New choice", store.read()!!.name)
    }

    private class Disk : ApplianceSelectionPersistence {
        var value: String? = null
        override val readable = true
        override fun read() = value
        override fun write(value: String): Boolean {this.value = value;return true}
    }
    private companion object {
        val PHONE = ApplianceIdentity("11111111-1111-4111-8111-111111111111", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE", "a".repeat(20))
        val PI = ApplianceIdentity("22222222-2222-4222-8222-222222222222", "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI", "b".repeat(20))
        val SELECTED = SelectedAppliance(PHONE, PI, "Pi")
        const val GENERATION = "44444444-4444-4444-8444-444444444444"
        const val TOKEN = "platform-local-bearer-" + "012345678901234567890123456789"
        fun envelope(): JSONObject = JSONObject().put("schemaVersion", 1).put("applianceId", PI.deviceId).put("controllerId", PHONE.deviceId)
            .put("bondGeneration", GENERATION).put("platformSupported", true).put("platformAvailable", true)
            .put("snapshot", JSONObject(AppliancePlatformTest::class.java.getResourceAsStream("/appliance-platform.json")!!.bufferedReader().use { it.readText() }))
        fun source(value: JSONObject): JSONObject = value.getJSONObject("snapshot").getJSONArray("sources").getJSONObject(0)
    }
}
