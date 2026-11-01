package life.michaelwong.covalent.appliance

import java.io.IOException
import java.security.MessageDigest
import java.util.UUID
import life.michaelwong.covalent.data.CovalentNodeClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

/** Structural/controller checks; the shared fixture contains real node-signed proof. */
class ApplianceUpdatesTest {
    private fun fixture() = JSONObject(javaClass.getResourceAsStream("/appliance-updates.json")!!.bufferedReader().use { it.readText() })
    private fun initial() = fixture().getJSONObject("initial")
    private fun selected(): SelectedAppliance {
        val value = initial(); val bond = value.getJSONObject("context").getJSONObject("bond")
        return SelectedAppliance(bond.getJSONObject("controller").identity(), bond.getJSONObject("appliance").identity(), "Example Pi", value.getString("bondGeneration"))
    }
    private fun saved(key: String = "pending") = parsePiSavedUpdate(fixture().getJSONObject(key), selected().local)
    @Test fun sharedCanonicalProofAndAppliedReceiptMatchRustExactly() {
        val value = saved(); assertEquals(value.payload, value.choice.canonicalPayload()); assertFalse(value.resolved)
        val applied = saved("applied"); assertTrue(applied.resolved); assertEquals(PiUpdateOutcome.APPLIED, applied.receipt!!.outcome)
        assertTrue(parsePiUpdates(fixture().getJSONObject("current"), selected()).context!!.enabled)
        assertFalse(parsePiUpdates(initial(), selected()).context!!.enabled)
        assertFalse(value.toString().contains(value.payload));assertFalse(value.choice.toString().contains(value.choice.id))
    }
    @Test fun unavailableUnconfiguredAndBusyStatesRemainDistinct() {
        val raw = initial();raw.put("updatesAvailable", false).put("context", JSONObject.NULL)
        assertNull(parsePiUpdates(raw, selected()).context)
        val unconfigured = initial();unconfigured.getJSONObject("context").put("configured", false).put("enabled", false).put("revision", JSONObject.NULL)
        assertFalse(parsePiUpdates(unconfigured, selected()).context!!.configured)
        val busy = initial();busy.getJSONObject("context").put("busy", true);assertTrue(parsePiUpdates(busy, selected()).context!!.busy)
    }
    @Test fun foreignStaleAmbiguousOrUnconfiguredEnabledContextsAreRefused() {
        val changes = listOf<(JSONObject) -> Unit>(
            { it.put("controllerId", UUID.randomUUID().toString()) }, { it.put("bondGeneration", UUID.randomUUID().toString()) },
            { it.put("updatesSupported", false) }, { it.remove("context") },
            { it.getJSONObject("context").put("token", "forbidden") }, { it.getJSONObject("context").remove("revision") },
            { it.getJSONObject("context").put("revision", UUID(0, 0).toString()) },
            { it.getJSONObject("context").put("configured", false) },
            { it.getJSONObject("context").put("busy", "false") },
            { it.getJSONObject("context").getJSONObject("bond").put("rootGeneration", UUID(0, 0).toString()) },
        )
        changes.forEach { change -> assertThrows(Exception::class.java) { parsePiUpdates(initial().also(change), selected()) } }
    }
    @Test fun notObservedDoesNotResolveAndNewRevisionNeverClaimsOriginalRequestApplied() {
        val raw = fixture().getJSONObject("pending"); val c = saved().choice
        val digest = MessageDigest.getInstance("SHA-256").digest(saved().payload.toByteArray()).joinToString("") { "%02x".format(it.toInt() and 255) }
        raw.put("receipt", JSONObject().put("schemaVersion",1).put("state","notObserved").put("actionId",c.id)
            .put("payloadDigest",digest).put("revision",c.revision).put("enabled",false))
        assertFalse(parsePiSavedUpdate(raw, selected().local).resolved)
        raw.put("observedContext", fixture().getJSONObject("current").getJSONObject("context"))
        val observed = parsePiSavedUpdate(raw, selected().local);assertTrue(observed.resolved);assertEquals(PiUpdateOutcome.NOT_OBSERVED, observed.receipt!!.outcome)
        raw.put("observedContext", initial().getJSONObject("context"));assertThrows(Exception::class.java) { parsePiSavedUpdate(raw,selected().local) }
    }
    @Test fun savedRequestsAndReceiptsRejectChangesMissingFieldsAndFalseSuccess() {
        val changes = listOf<(JSONObject) -> Unit>(
            { it.put("order",0) }, { it.put("token","forbidden") }, { it.remove("receipt") },
            { it.getJSONObject("proof").put("signature","invalid") }, { it.getJSONObject("proof").put("payload"," ".repeat(1281)) },
            { it.getJSONObject("receipt").put("actionId",UUID.randomUUID().toString()) },
            { it.getJSONObject("receipt").put("payloadDigest","a".repeat(64)) },
            { it.getJSONObject("receipt").put("enabled",false) },
            { it.getJSONObject("receipt").put("revision",saved().choice.revision) },
            { it.getJSONObject("receipt").put("state","notObserved") },
        )
        changes.forEach { change -> assertThrows(Exception::class.java) { parsePiSavedUpdate(fixture().getJSONObject("applied").also(change), selected().local) } }
        val raw=JSONObject().put("schemaVersion",1).put("actions",JSONArray().put(fixture().getJSONObject("pending")).put(fixture().getJSONObject("pending")))
        assertThrows(Exception::class.java) { parsePiUpdateActions(raw, selected().local) }
    }
    private class Disk : ApplianceSelectionPersistence {
        var value: String? = null; override val readable = true
        override fun read() = value; override fun write(value: String): Boolean {this.value=value;return true}
    }
    private inner class Fake : ApplianceUpdatesApi {
        var context = parsePiUpdates(initial(),selected()); var confirms=0;var checks=0;var offline=false
        val history=mutableListOf(saved());var beforeContext:(()->Unit)?=null
        override fun pairing() = ApplianceContext(selected().local,listOf(selected().appliance))
        override fun updateContext(selected:SelectedAppliance):PiUpdatesObservation {beforeContext?.invoke();if(offline)throw IOException("offline");return context}
        override fun updateActions(local:ApplianceIdentity)=history.toList()
        override fun confirmUpdate(choice:PiUpdateChoice):PiSavedUpdate {confirms++;return saved().copy(choice=choice,payload=choice.canonicalPayload())}
        override fun checkUpdate(action:PiSavedUpdate):PiSavedUpdate {checks++;return action}
    }
    private fun store() = ApplianceSelectionStore(Disk()).also {it.replace(null, selected())}
    @Test fun controllerUsesExplicitConfirmationAndRecoveryOnlyCallsLookup() {
        val api=Fake();val controller=ApplianceUpdatesController(store(),api);val selected=selected()
        val review=controller.review(selected,true);assertEquals(0,api.confirms)
        controller.confirm(review);assertEquals(1,api.confirms)
        controller.resolve(selected,saved());assertEquals(1,api.confirms);assertEquals(1,api.checks)
        controller.refresh(selected,false);assertEquals(1,api.confirms);assertEquals(1,api.checks)
    }
    @Test fun controllerRefusesChangedSelectionOrRevisionWithoutConfirmingAnything() {
        val api=Fake();val disk=store();val controller=ApplianceUpdatesController(disk,api);val review=controller.review(selected(),true)
        api.context=parsePiUpdates(fixture().getJSONObject("current"),selected())
        assertThrows(Exception::class.java) {controller.confirm(review)};assertEquals(0,api.confirms)
        api.context=parsePiUpdates(initial(),selected());val fresh=controller.review(selected(),true)
        api.beforeContext={disk.replace(selected(),selected().copy(name="Changed Pi choice"))}
        assertThrows(Exception::class.java) {controller.confirm(fresh)};assertEquals(0,api.confirms)
    }
    @Test fun busyOrUnconfiguredContextCannotCreateAReview() {
        val api=Fake();val controller=ApplianceUpdatesController(store(),api)
        api.context=api.context.copy(context=api.context.context!!.copy(busy=true))
        assertThrows(Exception::class.java) {controller.review(selected(),true)}
        api.context=api.context.copy(context=api.context.context!!.copy(busy=false,configured=false,enabled=false,revision=null))
        assertThrows(Exception::class.java) {controller.review(selected(),true)};assertEquals(0,api.confirms)
    }
    @Test fun clientUsesOnlyTheBoundedAuthenticatedFixedRoutes() {
        val server=MockWebServer();server.start()
        try {
            val client=CovalentNodeClient();val base=server.url("/").toString().removeSuffix("/");val token="t".repeat(48)
            server.enqueue(MockResponse().setBody(initial().toString()));client.applianceUpdates(base,token,selected())
            server.takeRequest().let {assertEquals("/api/v1/appliance/updates/context",it.path);assertEquals("Bearer $token",it.getHeader("Authorization"))}
            server.enqueue(MockResponse().setBody(fixture().getJSONObject("pending").toString()));client.confirmApplianceUpdate(base,token,saved().choice)
            server.takeRequest().let {assertEquals("/api/v1/appliance/updates/confirm",it.path);assertEquals(saved().payload,it.body.readUtf8())}
            server.enqueue(MockResponse().setBody(fixture().getJSONObject("applied").toString()));client.checkApplianceUpdate(base,token,saved())
            server.takeRequest().let {assertEquals("/api/v1/appliance/updates/check",it.path);assertEquals(saved().choice.id,JSONObject(it.body.readUtf8()).getString("actionId"))}
        } finally {server.shutdown()}
    }
}
