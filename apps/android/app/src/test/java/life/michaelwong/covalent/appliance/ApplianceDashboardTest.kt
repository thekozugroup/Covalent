package life.michaelwong.covalent.appliance

import java.io.IOException
import java.util.Base64
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.NodeApiException
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class ApplianceDashboardTest {
    @Test fun firstRunWaitsForExplicitPhoneSetupWithoutProbingOrForgettingASavedPi() {
        val disk = Disk(); val store = ApplianceSelectionStore(disk); val api = Api()
        val controller = ApplianceDashboardController(store, api)
        assertEquals(ApplianceConnectionState.NO_SELECTION, controller.refresh(phoneReady = false).connection)
        store.replace(null, SELECTED)
        val result = controller.refresh(phoneReady = false)
        assertEquals(ApplianceConnectionState.PHONE_UNAVAILABLE, result.connection)
        assertEquals(SELECTED, result.selection);assertEquals(0, api.contextCalls);assertEquals(0, api.statusCalls)
    }
    @Test fun clientUsesOnlyItsLocalBearerAndExactReadOnlyPeerRequest() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(contextJson().toString()))
            server.enqueue(MockResponse().setBody(statusJson().toString()))
            val client = CovalentNodeClient()
            val url = server.url("/").toString().removeSuffix("/")
            assertEquals(CONTEXT, client.applianceContext(url, TOKEN))
            assertEquals(OBSERVATION, client.applianceStatus(url, TOKEN, PI.deviceId, PHONE.deviceId))
            val first = server.takeRequest()
            assertEquals("GET", first.method); assertEquals("/api/v1/appliance/controller", first.path)
            assertEquals("Bearer $TOKEN", first.getHeader("Authorization"))
            val second = server.takeRequest()
            assertEquals("POST", second.method); assertEquals("/api/v1/appliance/status", second.path)
            assertEquals("{\"peerId\":\"${PI.deviceId}\"}", second.body.readUtf8())
            assertEquals("Bearer $TOKEN", second.getHeader("Authorization"))
        }
    }

    @Test fun boundedControlReadsRejectRedirectsAndOversizedBodies() {
        MockWebServer().use { server -> MockWebServer().use { other ->
            server.enqueue(MockResponse().setResponseCode(302).setHeader("Location", other.url("/")))
            other.enqueue(MockResponse().setBody(contextJson().toString()))
            val client = CovalentNodeClient(); val url = server.url("/").toString().removeSuffix("/")
            assertEquals(302, assertThrows(NodeApiException::class.java) { client.applianceContext(url, TOKEN) }.statusCode)
            assertEquals(0, other.requestCount)
            server.enqueue(MockResponse().setBody(" ".repeat(65_537)))
            assertThrows(Exception::class.java) { client.applianceContext(url, TOKEN) }
            server.enqueue(MockResponse().setBody(" ".repeat(8193)))
            assertThrows(Exception::class.java) { client.applianceStatus(url, TOKEN, PI.deviceId, PHONE.deviceId) }
        } }
    }

    @Test fun statusRefusesWrongIdentityMissingNullsUnknownAuthorityAndCoercions() {
        val bad = listOf<(JSONObject) -> Unit>(
            { it.remove("shareRecords") }, { it.put("shareRecords", -1) },
            { it.put("shareRecords", "1") }, { it.put("shareRecords", 1.5) },
            { it.put("schemaVersion", "1") }, { it.put("schemaVersion", 2) },
            { it.put("controllerId", PI.deviceId) }, { it.put("applianceId", OTHER.deviceId) },
            { it.put("bondGeneration", "00000000-0000-0000-0000-000000000000") },
            { it.put("syncAvailable", "true") }, { it.put("folderHealthFresh", "false") },
            { it.put("cloudBackupVerificationAvailable", true) }, { it.put("sourceCleanupAvailable", true) },
            { it.put("syncLifecycle", "complete") }, { it.put("extra", false) },
            { it.put("syncAvailable", false) }, { it.put("syncLifecycle", "unavailable") },
        )
        bad.forEach { change ->
            assertThrows(Exception::class.java) { parseApplianceObservation(statusJson().also(change), PI.deviceId, PHONE.deviceId) }
        }
        val unavailable = statusJson().put("syncAvailable", false).put("syncLifecycle", "unavailable")
            .put("shareRecords", JSONObject.NULL).put("folderHealthFresh", false)
        assertEquals(ApplianceSyncState.UNAVAILABLE, parseApplianceObservation(unavailable, PI.deviceId, PHONE.deviceId).syncState)
        assertThrows(Exception::class.java) {
            parseApplianceObservation(unavailable.put("folderHealthFresh", true), PI.deviceId, PHONE.deviceId)
        }
    }

    @Test fun contextRefusesMalformedKeysDuplicatePeersSelfAndMissingController() {
        val bad = listOf<(JSONObject) -> Unit>(
            { it.getJSONObject("bond").remove("controller") },
            { it.getJSONObject("bond").put("schemaVersion", "1") },
            { it.put("pairedDevices", JSONArray().put(identityJson(PI)).put(identityJson(PI))) },
            { it.put("pairedDevices", JSONArray().put(identityJson(PHONE))) },
            { it.getJSONArray("pairedDevices").getJSONObject(0).put("publicKey", "invalid") },
            { it.getJSONArray("pairedDevices").getJSONObject(0).put("publicKey", PI.publicKey + "=") },
            { it.getJSONArray("pairedDevices").getJSONObject(0).put("fingerprint", "invalid") },
            { it.getJSONObject("bond").put("controller", identityJson(PHONE)) },
            { it.put("unexpected", false) },
        )
        bad.forEach { change -> assertThrows(Exception::class.java) { parseApplianceContext(contextJson().also(change)) } }
        assertThrows(Exception::class.java) {
            parseApplianceContext(contextJson().put("pairedDevices", JSONArray().apply { repeat(129) { put(identityJson(PI)) } }))
        }
    }

    @Test fun selectionSurvivesReopenAndReadOnlyRefreshCannotChooseAPeerAutomatically() {
        val disk = Disk(); val store = ApplianceSelectionStore(disk); val api = Api()
        val controller = ApplianceDashboardController(store, api)
        assertEquals(ApplianceConnectionState.NO_SELECTION, controller.refresh().connection)
        assertNull(disk.value); assertEquals(0, api.statusCalls)
        controller.select(null, CONTEXT, ApplianceCandidate(PI, "Living room Pi"))
        val saved = ApplianceSelectionStore(disk).read()!!
        assertNull(saved.confirmedGeneration); assertEquals(PI, saved.appliance)
        assertEquals(ApplianceConnectionState.CONNECTED, controller.refresh().connection)
        assertEquals(GENERATION, ApplianceSelectionStore(disk).read()!!.confirmedGeneration)
        val before = disk.value
        assertEquals(ApplianceConnectionState.CONNECTED, controller.refresh().connection)
        assertEquals(before, disk.value)
        assertFalse(saved.toString().contains(PI.deviceId)); assertFalse(PI.toString().contains(PI.publicKey))
    }

    @Test fun outagesRefusalsAndUnsupportedVersionsKeepTheFullSavedChoice() {
        val disk = Disk(); val store = ApplianceSelectionStore(disk);store.replace(null, SELECTED)
        val original = disk.value
        val errors = listOf(
            IOException("private network detail") to ApplianceConnectionState.UNAVAILABLE,
            NodeApiException(503, 1, "controller_unavailable", false, "private") to ApplianceConnectionState.UNAVAILABLE,
            NodeApiException(403, 1, "controller_rejected", false, "private", hasExplicitRetryContract = true) to ApplianceConnectionState.APPROVAL_REQUIRED,
            NodeApiException(403, 1, "controller_rejected", false, "legacy") to ApplianceConnectionState.UNAVAILABLE,
            NodeApiException(404, 1, "http_404", false, "private") to ApplianceConnectionState.UNSUPPORTED,
        )
        for ((error, state) in errors) {
            val result = ApplianceDashboardController(store, Api().apply { statusError = error }).refresh()
            assertEquals(state, result.connection); assertEquals(SELECTED, result.selection)
            assertNull(result.observation); assertEquals(original, disk.value)
        }
        val localUnavailable = ApplianceDashboardController(store, Api().apply { contextError = IOException() }).refresh()
        assertEquals(ApplianceConnectionState.PHONE_UNAVAILABLE, localUnavailable.connection)
        assertEquals(SELECTED, localUnavailable.selection); assertEquals(original, disk.value)
    }

    @Test fun changedOrRevokedFullKeysStopBeforeStatusWithoutReplacingChoice() {
        for (context in listOf(CONTEXT.copy(local = PHONE.copy(publicKey = OTHER.publicKey)),
            CONTEXT.copy(paired = listOf(PI.copy(publicKey = OTHER.publicKey))), CONTEXT.copy(paired = emptyList()))) {
            val disk = Disk(); val store = ApplianceSelectionStore(disk);store.replace(null, SELECTED)
            val before = disk.value; val api = Api().apply { current = context }
            val state = ApplianceDashboardController(store, api).refresh()
            assertEquals(ApplianceConnectionState.IDENTITY_CHANGED, state.connection)
            assertEquals(0, api.statusCalls); assertEquals(before, disk.value)
        }
    }

    @Test fun staleStatusAndFailedSaveNeverPublishConnectedOrReplaceNewerChoice() {
        for (failedSave in listOf(false, true)) {
            val disk = Disk(); val store = ApplianceSelectionStore(disk);store.replace(null, SELECTED)
            val replacement = SELECTED.copy(appliance = OTHER, name = "Another Pi")
            val api = Api().apply { beforeStatus = {
                if (failedSave) disk.failWrites = true else store.replace(SELECTED, replacement)
            } }
            val state = ApplianceDashboardController(store, api).refresh()
            assertEquals(ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE, state.connection)
            assertNull(state.observation)
            assertEquals(if (failedSave) SELECTED else replacement, ApplianceSelectionStore(disk).read())
        }
    }

    @Test fun selectionRechecksPairingAndExpectedChoiceAndNeverUsesAddress() {
        val disk = Disk(); val store = ApplianceSelectionStore(disk);val api = Api()
        val controller = ApplianceDashboardController(store, api)
        val changed = CONTEXT.copy(paired = listOf(PI.copy(publicKey = OTHER.publicKey)))
        api.current = changed
        assertThrows(Exception::class.java) { controller.select(null, CONTEXT, ApplianceCandidate(PI, "Pi")) }
        assertNull(disk.value)
        api.current = CONTEXT
        store.replace(null, SELECTED)
        assertThrows(Exception::class.java) { controller.select(null, CONTEXT, ApplianceCandidate(PI, "Pi")) }
        assertEquals(SELECTED, store.read());assertEquals(0, api.statusCalls)
        assertFalse(disk.value!!.contains("address"));assertFalse(disk.value!!.contains(TOKEN))
    }

    @Test fun lockedDamagedAndFutureSavedChoicesNeverBecomeAnEmptySetup() {
        for (raw in listOf("", "{}", " ".repeat(8193), "{\"schemaVersion\":2}")) {
            val disk = Disk().apply { value = raw };val api = Api()
            assertEquals(ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE,
                ApplianceDashboardController(ApplianceSelectionStore(disk), api).refresh().connection)
            assertEquals(0, api.contextCalls); assertEquals(raw, disk.value)
        }
        val disk = Disk();val store = ApplianceSelectionStore(disk);store.replace(null, SELECTED)
        val before = disk.value;disk.readable = false
        assertThrows(Exception::class.java) { store.replace(SELECTED, SELECTED.copy(name = "New")) }
        assertEquals(ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE, ApplianceDashboardController(store, Api()).refresh().connection)
        assertEquals(before, disk.value)
    }

    private class Disk : ApplianceSelectionPersistence {
        override var readable = true
        var value: String? = null
        var failWrites = false
        override fun read() = value
        override fun write(value: String): Boolean { if (failWrites) return false;this.value = value;return true }
    }
    private class Api : ApplianceDashboardApi {
        var current = CONTEXT
        var contextError: Exception? = null
        var statusError: Exception? = null
        var beforeStatus: () -> Unit = {}
        var contextCalls = 0
        var statusCalls = 0
        override fun context(): ApplianceContext { contextCalls++;contextError?.let { throw it };return current }
        override fun names() = mapOf(PI.deviceId to "Living room Pi")
        override fun status(peerId: String, localId: String): ApplianceObservation {
            statusCalls++;beforeStatus();statusError?.let { throw it }
            assertEquals(PI.deviceId, peerId);assertEquals(PHONE.deviceId, localId)
            return OBSERVATION
        }
    }
    private companion object {
        const val TOKEN = "synthetic-local-token-at-least-thirty-two-characters"
        const val GENERATION = "44444444-4444-4444-8444-444444444444"
        val PHONE = identity("11111111-1111-4111-8111-111111111111", 1)
        val PI = identity("22222222-2222-4222-8222-222222222222", 2)
        val OTHER = identity("33333333-3333-4333-8333-333333333333", 3)
        val CONTEXT = ApplianceContext(PHONE, listOf(PI))
        val OBSERVATION = ApplianceObservation(GENERATION, ApplianceSyncState.RUNNING, 3, true)
        val SELECTED = SelectedAppliance(PHONE, PI, "Living room Pi")
        fun identity(id: String, byte: Int) = ApplianceIdentity(id,
            Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { byte.toByte() }), "a".repeat(20))
        fun identityJson(value: ApplianceIdentity) = JSONObject().put("deviceId", value.deviceId)
            .put("publicKey", value.publicKey).put("fingerprint", value.fingerprint)
        fun contextJson() = JSONObject().put("bond", JSONObject().put("schemaVersion", 1)
            .put("generation", GENERATION).put("appliance", identityJson(PHONE)).put("controller", JSONObject.NULL))
            .put("pairedDevices", JSONArray().put(identityJson(PI)))
        fun statusJson() = JSONObject().put("schemaVersion", 1).put("applianceId", PI.deviceId)
            .put("controllerId", PHONE.deviceId).put("bondGeneration", GENERATION).put("syncAvailable", true)
            .put("syncLifecycle", "running").put("shareRecords", 3).put("folderHealthFresh", true)
            .put("cloudBackupVerificationAvailable", false).put("sourceCleanupAvailable", false)
    }
}
