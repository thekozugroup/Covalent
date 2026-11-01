package life.michaelwong.covalent.appliance

import java.security.MessageDigest
import java.util.Base64
import java.util.UUID
import org.json.JSONObject

internal data class PiUpdateContext(
    val bond: PiImportBond, val configured: Boolean, val enabled: Boolean,
    val revision: String?, val busy: Boolean,
) {
    init {
        revision?.let(::requireCanonicalId)
        require(configured == (revision != null) && (configured || !enabled))
    }
    fun obsoletes(choice: PiUpdateChoice) = bond.appliance == choice.bond.appliance && bond.controller == choice.bond.controller &&
        (bond != choice.bond || revision != null && revision != choice.revision)
    override fun toString() = "PiUpdateContext(<redacted>)"
}
internal data class PiUpdatesObservation(val generation: String, val supported: Boolean, val context: PiUpdateContext?)
internal data class PiUpdateChoice(val bond: PiImportBond, val id: String, val revision: String, val enabled: Boolean) {
    init { requireCanonicalId(id); requireCanonicalId(revision) }
    /** ASCII validation and field order match shared core. Only the local node signs. */
    fun canonicalPayload(): String {
        fun identity(v: ApplianceIdentity) = "{\"deviceId\":\"${v.deviceId}\",\"publicKey\":\"${v.publicKey}\",\"fingerprint\":\"${v.fingerprint}\"}"
        val b = "{\"appliance\":${identity(bond.appliance)},\"controller\":${identity(bond.controller)},\"rootGeneration\":\"${bond.rootGeneration}\",\"nativeGeneration\":\"${bond.nativeGeneration}\"}"
        return "{\"schemaVersion\":1,\"bond\":$b,\"actionId\":\"$id\",\"expectedRevision\":\"$revision\",\"enabled\":$enabled}"
    }
    override fun toString() = "PiUpdateChoice(<redacted>)"
}
internal enum class PiUpdateOutcome { APPLIED, NOT_OBSERVED }
internal data class PiUpdateReceipt(val outcome: PiUpdateOutcome, val revision: String, val enabled: Boolean)
internal data class PiSavedUpdate(
    val order: Long, val choice: PiUpdateChoice, val payload: String, val signature: String,
    val receipt: PiUpdateReceipt?, val observedContext: PiUpdateContext?,
) {
    val resolved get() = receipt?.outcome == PiUpdateOutcome.APPLIED || observedContext != null
    override fun toString() = "PiSavedUpdate(<redacted>)"
}
private fun parseUpdateContext(value: JSONObject): PiUpdateContext {
    value.exactKeys("schemaVersion", "bond", "configured", "enabled", "revision", "busy")
    require(value.strictLong("schemaVersion") == 1L)
    return PiUpdateContext(parseImportBond(value.getJSONObject("bond")), value.strictBoolean("configured"), value.strictBoolean("enabled"),
        if (value.isNull("revision")) null else value.strictString("revision"), value.strictBoolean("busy"))
}
internal fun parsePiUpdates(value: JSONObject, selected: SelectedAppliance): PiUpdatesObservation {
    value.exactKeys("schemaVersion", "applianceId", "controllerId", "bondGeneration", "updatesSupported", "updatesAvailable", "context")
    require(value.strictLong("schemaVersion") == 1L && value.strictString("applianceId") == selected.appliance.deviceId &&
        value.strictString("controllerId") == selected.local.deviceId)
    val generation = value.strictString("bondGeneration").also(::requireCanonicalId)
    require(generation == selected.confirmedGeneration)
    val supported = value.strictBoolean("updatesSupported"); val available = value.strictBoolean("updatesAvailable")
    require((supported || !available) && available != value.isNull("context"))
    val context = if (!available) null else parseUpdateContext(value.getJSONObject("context")).also {
        require(it.bond.appliance == selected.appliance && it.bond.controller == selected.local && it.bond.nativeGeneration == generation)
    }
    return PiUpdatesObservation(generation, supported, context)
}
internal fun parsePiSavedUpdate(value: JSONObject, local: ApplianceIdentity): PiSavedUpdate {
    value.exactKeys("order", "proof", "receipt", "observedContext")
    val order = value.strictLong("order").also { require(it > 0) }
    val proof = value.getJSONObject("proof"); proof.exactKeys("payload", "signature")
    val payload = proof.strictString("payload").also { require(it.toByteArray(Charsets.UTF_8).size <= 1280) }
    val signature = proof.strictString("signature").also {
        val bytes = Base64.getUrlDecoder().decode(it)
        require(bytes.size == 64 && Base64.getUrlEncoder().withoutPadding().encodeToString(bytes) == it)
    }
    val c = JSONObject(payload); c.exactKeys("schemaVersion", "bond", "actionId", "expectedRevision", "enabled")
    require(c.strictLong("schemaVersion") == 1L)
    val choice = PiUpdateChoice(parseImportBond(c.getJSONObject("bond")), c.strictString("actionId"), c.strictString("expectedRevision"), c.strictBoolean("enabled"))
    require(choice.bond.controller == local && choice.canonicalPayload() == payload)
    val receipt = if (value.isNull("receipt")) null else value.getJSONObject("receipt").let {
        it.exactKeys("schemaVersion", "state", "actionId", "payloadDigest", "revision", "enabled")
        require(it.strictLong("schemaVersion") == 1L && it.strictString("actionId") == choice.id)
        val digest = MessageDigest.getInstance("SHA-256").digest(payload.toByteArray(Charsets.UTF_8))
            .joinToString("") { b -> "%02x".format(b.toInt() and 255) }
        require(it.strictString("payloadDigest") == digest)
        val outcome = when (it.strictString("state")) { "applied" -> PiUpdateOutcome.APPLIED; "notObserved" -> PiUpdateOutcome.NOT_OBSERVED; else -> error("Invalid update receipt.") }
        val revision = it.strictString("revision").also(::requireCanonicalId); val enabled = it.strictBoolean("enabled")
        require(if (outcome == PiUpdateOutcome.APPLIED) enabled == choice.enabled && revision != choice.revision else revision == choice.revision)
        PiUpdateReceipt(outcome, revision, enabled)
    }
    val observation = if (value.isNull("observedContext")) null else parseUpdateContext(value.getJSONObject("observedContext")).also { require(it.obsoletes(choice)) }
    return PiSavedUpdate(order, choice, payload, signature, receipt, observation)
}
internal fun parsePiUpdateActions(value: JSONObject, local: ApplianceIdentity): List<PiSavedUpdate> {
    value.exactKeys("schemaVersion", "actions"); require(value.strictLong("schemaVersion") == 1L)
    val items = value.getJSONArray("actions"); require(items.length() <= 32)
    val actions = List(items.length()) { parsePiSavedUpdate(items.getJSONObject(it), local) }
    require(actions.map { it.order }.toSet().size == actions.size && actions.map { it.choice.id }.toSet().size == actions.size)
    return actions.sortedByDescending { it.order }
}

internal data class PiUpdateReview(val selected: SelectedAppliance, val context: PiUpdateContext, val choice: PiUpdateChoice)
internal data class PiUpdatesState(
    val selected: SelectedAppliance? = null, val observation: PiUpdatesObservation? = null,
    val actions: List<PiSavedUpdate> = emptyList(), val actionsAvailable: Boolean = false,
)
internal interface ApplianceUpdatesApi {
    fun pairing(): ApplianceContext
    fun updateContext(selected: SelectedAppliance): PiUpdatesObservation
    fun updateActions(local: ApplianceIdentity): List<PiSavedUpdate>
    fun confirmUpdate(choice: PiUpdateChoice): PiSavedUpdate
    fun checkUpdate(action: PiSavedUpdate): PiSavedUpdate
}
/** Confirmation signs once through the node's durable outbox. Recovery never calls confirm. */
internal class ApplianceUpdatesController(private val store: ApplianceSelectionStore, private val api: ApplianceUpdatesApi) {
    private fun current(selected: SelectedAppliance) {
        check(store.read() == selected)
        val pairing = api.pairing()
        check(pairing.local == selected.local && selected.appliance in pairing.paired && store.read() == selected)
    }
    fun refresh(selected: SelectedAppliance, connected: Boolean): PiUpdatesState {
        current(selected)
        val before = api.updateActions(selected.local)
        val observation = if (connected) api.updateContext(selected) else null
        // A fresh context may retire an obsolete request without asserting its past outcome.
        val actions = (if (connected) api.updateActions(selected.local) else before).filter { it.choice.bond.appliance == selected.appliance }
        current(selected)
        return PiUpdatesState(selected, observation, actions, true)
    }
    fun review(selected: SelectedAppliance, enabled: Boolean): PiUpdateReview {
        current(selected)
        val context = checkNotNull(api.updateContext(selected).context)
        check(context.configured && !context.busy && context.bond.appliance == selected.appliance && context.bond.controller == selected.local &&
            context.bond.nativeGeneration == selected.confirmedGeneration)
        current(selected)
        return PiUpdateReview(selected, context, PiUpdateChoice(context.bond, UUID.randomUUID().toString(), checkNotNull(context.revision), enabled))
    }
    fun confirm(review: PiUpdateReview): PiSavedUpdate {
        current(review.selected)
        val fresh = checkNotNull(api.updateContext(review.selected).context)
        check(fresh == review.context && fresh.configured && !fresh.busy)
        current(review.selected)
        val saved = api.confirmUpdate(review.choice)
        check(saved.choice == review.choice)
        current(review.selected)
        return saved
    }
    fun resolve(selected: SelectedAppliance, action: PiSavedUpdate): PiSavedUpdate {
        current(selected)
        check(action.choice.bond.appliance == selected.appliance && action.choice.bond.controller == selected.local)
        val result = api.checkUpdate(action)
        check(result.choice == action.choice && result.payload == action.payload && result.signature == action.signature && result.order == action.order)
        current(selected)
        return result
    }
}
