package life.michaelwong.covalent.appliance

import java.io.IOException
import java.security.MessageDigest
import java.util.UUID
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.NodeApiException
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

/** Structural Android contract; the fixture's real signatures were verified by shared core/Android native tests. */
class ApplianceImportsTest {
    @Test fun sharedCoreCanonicalPayloadAndAllReceiptStatesDecodeExactly() {
        val saved = saved(); assertEquals(action().getJSONObject("proof").getString("payload"), saved.choice.canonicalPayload())
        assertNull(saved.outcome);assertFalse(saved.resolved)
        for (outcome in PiImportOutcome.entries) {
            val decoded = parsePiSavedImport(withReceipt(action(), outcome), PHONE)
            assertEquals(outcome, decoded.outcome)
            assertEquals(outcome in setOf(PiImportOutcome.ACCEPTED, PiImportOutcome.EXPIRED, PiImportOutcome.WITHDRAWN), decoded.resolved)
        }
        val observation = parsePiImports(envelope(), SELECTED)
        assertEquals(5L, observation.context!!.sources.single().source.bytes)
        assertEquals(8_500_000_000L, observation.context.spaceAfterReserve)
        assertNull(parsePiImports(envelope().put("importsAvailable", false).put("context", JSONObject.NULL), SELECTED).context)
    }

    @Test fun contextRejectsChangedOwnersGenerationsUnsafeSourcesAndIncompleteConfirmations() {
        val mutations = listOf<(JSONObject) -> Unit>(
            { it.put("controllerId", PI.deviceId) }, { it.put("bondGeneration", UUID.randomUUID().toString()) },
            { it.put("importsSupported", false) }, { it.put("importsAvailable", false) }, { it.remove("context") },
            { it.getJSONObject("context").put("withdrawalAvailable", false) },
            { it.getJSONObject("context").put("sourceCleanupAvailable", true) },
            { it.getJSONObject("context").getJSONObject("bond").getJSONObject("appliance").put("publicKey", PHONE.publicKey) },
            { it.getJSONObject("context").getJSONObject("bond").put("rootGeneration", UUID(0, 0).toString()) },
            { it.getJSONObject("context").put("storage", JSONObject.NULL) },
            { it.getJSONObject("context").getJSONObject("storage").put("availableAfterReserveBytes", 4) },
            { it.getJSONObject("context").put("volumeServiceAvailable", false) },
            { source(it).put("name", "\u202eprivate") }, { source(it).put("kind", "camera") },
            { source(it).put("destination", "/data/private") }, { source(it).put("actionSequence", "1") },
            { source(it).put("actionSequence", 0) }, { source(it).remove("confirmation") },
            { source(it).getJSONObject("confirmation").put("previewId", UUID(0, 0).toString()) },
            { source(it).getJSONObject("preview").put("state", "incomplete") },
            { it.getJSONObject("context").getJSONArray("sources").put(source(it)) },
        )
        mutations.forEach { change -> assertThrows(Exception::class.java) { parsePiImports(envelope().also(change), SELECTED) } }
        val disconnected = envelope();source(disconnected).put("connected", false).put("confirmation", JSONObject.NULL)
            .put("preview", JSONObject().put("state", "unavailable").put("counts", JSONObject.NULL))
        assertNull(parsePiImports(disconnected, SELECTED).context!!.sources.single().confirmation)
    }

    @Test fun savedRequestsRejectAmbiguityChangedTuplesDigestsAndForeignPhoneIdentity() {
        val mutations = listOf<(JSONObject) -> Unit>(
            { it.put("order", 0) }, { it.put("withdrawalRequested", "false") }, { it.remove("receipt") },
            { it.put("withdrawalRequested", true) }, { it.getJSONObject("proof").put("signature", "bad") },
            { it.getJSONObject("proof").put("payload", " " + it.getJSONObject("proof").getString("payload")) },
            { it.getJSONObject("proof").put("payload", it.getJSONObject("proof").getString("payload").replace("\"schemaVersion\":1", "\"schemaVersion\":1,\"schemaVersion\":1")) },
            { it.getJSONObject("source").put("actionSequence", 2) },
            { it.getJSONObject("source").getJSONObject("confirmation").put("previewId", UUID.randomUUID().toString()) },
            { withReceipt(it, PiImportOutcome.ACCEPTED).getJSONObject("receipt").put("choiceDigest", "0".repeat(64)) },
            { withReceipt(it, PiImportOutcome.ACCEPTED).getJSONObject("receipt").put("actionSequence", 2) },
            { withReceipt(it, PiImportOutcome.UNKNOWN).getJSONObject("receipt").put("actionSession", saved().choice.session) },
            { withReceipt(it, PiImportOutcome.ACCEPTED).getJSONObject("receipt").put("admissionOnly", false) },
        )
        mutations.forEach { change -> assertThrows(Exception::class.java) { parsePiSavedImport(action().also(change), PHONE) } }
        assertThrows(Exception::class.java) { parsePiSavedImport(action(), PI) }
        val withdrawn = action().put("withdrawalRequested", true).put("withdrawalSignature", fixture().getString("withdrawalSignature"))
        assertTrue(parsePiSavedImport(withdrawn, PHONE).withdrawalRequested)
    }

    @Test fun onlyALaterAcknowledgedPauseCanSupersedeAnUnresolvedRequest() {
        val earlier = action();val later = action();val original = saved().choice
        val pause = original.copy(id = UUID.randomUUID().toString(), mode = PiImportMode.OFF, confirmation = null, sequence = 2)
        later.put("order", 2).getJSONObject("proof").put("payload", pause.canonicalPayload())
        later.getJSONObject("source").put("actionSequence", 2)
        withReceipt(later, PiImportOutcome.ACCEPTED)
        earlier.put("supersededBy", pause.id)
        val list = JSONObject().put("schemaVersion", 1).put("actions", JSONArray().put(earlier).put(later))
        assertTrue(parsePiImportActions(list, PHONE).all { it.resolved })
        later.put("receipt", JSONObject.NULL)
        assertThrows(Exception::class.java) { parsePiImportActions(list, PHONE) }
        assertThrows(Exception::class.java) { parsePiImportActions(JSONObject().put("schemaVersion", 1).put("actions", JSONArray().put(action()).put(action())), PHONE) }
    }

    @Test fun clientUsesFiveExactLocalRoutesBearerBoundsAndNoRedirects() {
        MockWebServer().use { server -> MockWebServer().use { other ->
            val client = CovalentNodeClient();val url = server.url("/").toString().removeSuffix("/");val pending = saved()
            server.enqueue(MockResponse().setBody(envelope().toString()));client.applianceImports(url, TOKEN, SELECTED)
            server.enqueue(MockResponse().setBody(JSONObject().put("schemaVersion", 1).put("actions", JSONArray().put(action())).toString()));client.applianceImportActions(url, TOKEN, PHONE)
            server.enqueue(MockResponse().setBody(action().toString()));client.confirmApplianceImport(url, TOKEN, pending.choice)
            server.enqueue(MockResponse().setBody(withReceipt(action(), PiImportOutcome.ACCEPTED).toString()));client.checkApplianceImport(url, TOKEN, pending)
            server.enqueue(MockResponse().setBody(action().put("withdrawalRequested", true).put("withdrawalSignature", fixture().getString("withdrawalSignature")).toString()));client.withdrawApplianceImport(url, TOKEN, pending)
            listOf("context", "actions", "confirm", "check", "withdraw").forEach { path ->
                val request = server.takeRequest();assertEquals("/api/v1/appliance/imports/$path", request.path)
                assertEquals(if (path == "actions") "GET" else "POST", request.method)
                assertEquals("Bearer $TOKEN", request.getHeader("Authorization"))
                when (path) {
                    "context" -> assertEquals(PI.deviceId, JSONObject(request.body.readUtf8()).getString("peerId"))
                    "confirm" -> assertEquals(pending.payload, request.body.readUtf8())
                    "check", "withdraw" -> assertEquals(setOf("actionId"), JSONObject(request.body.readUtf8()).keys().asSequence().toSet())
                }
            }
            server.enqueue(MockResponse().setResponseCode(302).setHeader("Location", other.url("/")))
            assertThrows(NodeApiException::class.java) { client.applianceImports(url, TOKEN, SELECTED) };assertEquals(0, other.requestCount)
            server.enqueue(MockResponse().setBody(" ".repeat(131_073)))
            assertThrows(Exception::class.java) { client.applianceImportActions(url, TOKEN, PHONE) }
            server.enqueue(MockResponse().setBody(action().toString()))
            assertThrows(Exception::class.java) { client.confirmApplianceImport(url, TOKEN, pending.choice.copy(id = UUID.randomUUID().toString())) }
        } }
    }

    @Test fun reviewWritesNothingAndDefaultsRemainExplicitlyOnceOrRemember() {
        val f = ControllerFixture();val review = f.controller.review(SELECTED, saved().choice.sourceId, false)
        assertEquals(0, f.api.confirms);assertEquals(1L, review.source.source.count)
        assertEquals(PiImportMode.ONCE, review.choice(PiImportMode.ONCE).mode)
        assertEquals(PiImportMode.REMEMBER, review.choice(PiImportMode.REMEMBER).mode)
        assertThrows(Exception::class.java) { review.choice(PiImportMode.OFF) }
        val pause = f.controller.review(SELECTED, saved().choice.sourceId, true)
        assertEquals(PiImportMode.OFF, pause.choice(PiImportMode.OFF).mode)
        assertThrows(Exception::class.java) { pause.choice(PiImportMode.ONCE) }
    }

    @Test fun changedAttachmentOwnerSelectionOrPairingRefusesBeforeConfirmation() {
        val f = ControllerFixture();val review = f.controller.review(SELECTED, saved().choice.sourceId, false)
        val context = f.api.observation.context!!
        f.api.observation = f.api.observation.copy(context = context.copy(sources = context.sources.map { it.copy(confirmation = it.confirmation!!.copy(previewId = UUID.randomUUID().toString())) }))
        assertThrows(Exception::class.java) { f.controller.confirm(review, PiImportMode.ONCE) };assertEquals(0, f.api.confirms)
        f.api.observation = parsePiImports(envelope(), SELECTED)
        f.api.paired = false
        assertThrows(Exception::class.java) { f.controller.confirm(review, PiImportMode.ONCE) };assertEquals(0, f.api.confirms)
        f.api.paired = true;f.store.replace(SELECTED, SELECTED.copy(name = "Changed selection"))
        assertThrows(Exception::class.java) { f.controller.confirm(review, PiImportMode.REMEMBER) };assertEquals(0, f.api.confirms)
    }

    @Test fun lostReplyAndControllerRecreationReadSavedStateWithoutRepeatingAnImport() {
        val f = ControllerFixture();val review = f.controller.review(SELECTED, saved().choice.sourceId, false)
        f.api.lose = true
        assertThrows(IOException::class.java) { f.controller.confirm(review, PiImportMode.ONCE) };assertEquals(1, f.api.confirms)
        val recreated = ApplianceImportsController(f.store, f.api)
        val offline = recreated.refresh(SELECTED, false);assertNull(offline.observation);assertTrue(offline.pending)
        assertEquals(1, f.api.confirms)
        recreated.resolve(SELECTED, offline.actions.single(), false)
        assertEquals(1, f.api.checks);assertEquals(1, f.api.confirms)
    }

    @Test fun newProtectedApprovalCanReviewWhileAnOldResultRemainsUnknown() {
        val f = ControllerFixture();f.api.actions = listOf(saved())
        assertThrows(Exception::class.java) { f.controller.review(SELECTED, saved().choice.sourceId, false) }
        val old = f.api.observation.context!!
        f.api.observation = f.api.observation.copy(context = old.copy(bond = old.bond.copy(rootGeneration = UUID.randomUUID().toString())))
        val reviewed = f.controller.review(SELECTED, saved().choice.sourceId, false)
        assertNotEquals(old.bond, reviewed.context.bond)
        assertNull(f.api.actions.single().outcome);assertFalse(f.api.actions.single().resolved);assertEquals(0, f.api.confirms)
    }

    private class Disk : ApplianceSelectionPersistence {
        var value: String? = null
        override val readable = true
        override fun read() = value
        override fun write(value: String): Boolean { this.value = value;return true }
    }
    private class FakeApi : ApplianceImportsApi {
        var confirms = 0;var checks = 0;var lose = false;var paired = true
        var observation = parsePiImports(envelope(), SELECTED)
        var actions = emptyList<PiSavedImport>()
        override fun pairing() = ApplianceContext(PHONE, if (paired) listOf(PI) else emptyList())
        override fun context(selected: SelectedAppliance) = observation
        override fun actions(local: ApplianceIdentity) = actions
        override fun confirm(choice: PiImportChoice): PiSavedImport {
            confirms++
            val value = action();value.getJSONObject("proof").put("payload", choice.canonicalPayload())
            val saved = parsePiSavedImport(value, PHONE);actions = listOf(saved)
            if (lose) throw IOException("Lost after durable admission")
            return saved
        }
        override fun check(action: PiSavedImport): PiSavedImport { checks++;return action }
        override fun withdraw(action: PiSavedImport) = action.copy(withdrawalRequested = true)
    }
    private class ControllerFixture {
        val store = ApplianceSelectionStore(Disk()).also { it.replace(null, SELECTED) }
        val api = FakeApi()
        val controller = ApplianceImportsController(store, api)
    }
    private companion object {
        fun fixture() = JSONObject(ApplianceImportsTest::class.java.getResourceAsStream("/appliance-imports.json")!!.bufferedReader().use { it.readText() })
        fun action() = fixture().getJSONObject("action")
        val PHONE = fixture().getJSONObject("context").getJSONObject("bond").getJSONObject("controller").identity()
        val PI = fixture().getJSONObject("context").getJSONObject("bond").getJSONObject("appliance").identity()
        val SELECTED = SelectedAppliance(PHONE, PI, "Pi", fixture().getJSONObject("context").getJSONObject("bond").getString("nativeGeneration"))
        const val TOKEN = "imports-local-bearer-012345678901234567890123456789"
        fun saved() = parsePiSavedImport(action(), PHONE)
        fun envelope() = JSONObject().put("schemaVersion", 1).put("applianceId", PI.deviceId).put("controllerId", PHONE.deviceId)
            .put("bondGeneration", SELECTED.confirmedGeneration).put("importsSupported", true).put("importsAvailable", true).put("context", fixture().getJSONObject("context"))
        fun source(value: JSONObject) = value.getJSONObject("context").getJSONArray("sources").getJSONObject(0)
        fun withReceipt(value: JSONObject, outcome: PiImportOutcome): JSONObject {
            val payload = value.getJSONObject("proof").getString("payload");val choice = JSONObject(payload)
            val receipt = JSONObject().put("actionId", choice.get("actionId"))
                .put("actionSession", if (outcome == PiImportOutcome.UNKNOWN) JSONObject.NULL else choice.get("actionSession"))
                .put("actionSequence", if (outcome == PiImportOutcome.UNKNOWN) JSONObject.NULL else choice.get("actionSequence"))
                .put("outcome", outcome.wire).put("admissionOnly", true)
                .put("choiceDigest", MessageDigest.getInstance("SHA-256").digest(payload.toByteArray()).joinToString("") { "%02x".format(it.toInt() and 255) })
            return value.put("receipt", receipt)
        }
    }
}
