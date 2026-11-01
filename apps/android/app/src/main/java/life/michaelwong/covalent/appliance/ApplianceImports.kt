package life.michaelwong.covalent.appliance

import java.security.MessageDigest
import java.util.Base64
import java.util.UUID
import org.json.JSONObject

internal data class PiImportBond(
    val appliance: ApplianceIdentity, val controller: ApplianceIdentity,
    val rootGeneration: String, val nativeGeneration: String,
) {
    init {
        requireCanonicalId(rootGeneration); requireCanonicalId(nativeGeneration)
        require(appliance.deviceId != controller.deviceId && appliance.publicKey != controller.publicKey)
    }
    override fun toString() = "PiImportBond(<redacted>)"
}
internal data class PiImportConfirmation(val attachmentId: String, val previewId: String) {
    init { requireCanonicalId(attachmentId); requireCanonicalId(previewId) }
}
internal data class PiImportControlSource(
    val source: PiImportSource, val session: String, val sequence: Long, val confirmation: PiImportConfirmation?,
    val canRemember: Boolean = true,
)
internal data class PiImportContext(
    val bond: PiImportBond, val volumesAvailable: Boolean, val truncated: Boolean,
    val sources: List<PiImportControlSource>, val spaceAfterReserve: Long?,
    val camerasAvailable: Boolean? = null,
)
internal data class PiImportsObservation(val generation: String, val supported: Boolean, val context: PiImportContext?)
internal enum class PiImportMode(val wire: String) { ONCE("once"), REMEMBER("remember"), OFF("off") }
internal enum class PiImportOutcome(val wire: String) {
    UNKNOWN("unknown"), ACCEPTED("accepted"), UNCERTAIN("uncertain"), EXPIRED("expired"), WITHDRAWN("withdrawn"),
}
internal data class PiImportChoice(
    val bond: PiImportBond, val id: String, val session: String, val sequence: Long,
    val sourceId: String, val mode: PiImportMode, val confirmation: PiImportConfirmation?,
    val camera: Boolean = false,
) {
    init {
        requireCanonicalId(id); requireCanonicalId(session)
        require(sequence > 0 && sourceId.matches(Regex("[0-9a-f]{64}")))
        require((mode == PiImportMode.OFF) == (confirmation == null))
    }
    /** All values are validated ASCII; order/framing matches the shared core. No signing here. */
    fun canonicalPayload(): String {
        fun identity(value: ApplianceIdentity) = "{\"deviceId\":\"${value.deviceId}\",\"publicKey\":\"${value.publicKey}\",\"fingerprint\":\"${value.fingerprint}\"}"
        val bondJson = "{\"appliance\":${identity(bond.appliance)},\"controller\":${identity(bond.controller)},\"rootGeneration\":\"${bond.rootGeneration}\",\"nativeGeneration\":\"${bond.nativeGeneration}\"}"
        val attachment = confirmation?.let { "\"${it.attachmentId}\"" } ?: "null"
        val preview = confirmation?.let { "\"${it.previewId}\"" } ?: "null"
        return "{\"schemaVersion\":${if (camera) 2 else 1},\"bond\":$bondJson,\"actionId\":\"$id\",\"actionSession\":\"$session\",\"actionSequence\":$sequence,\"sourceId\":\"$sourceId\",\"kind\":\"${if (camera) "camera" else "volume"}\",\"mode\":\"${mode.wire}\",\"attachmentId\":$attachment,\"previewId\":$preview,\"retention\":\"retain\"}"
    }
    override fun toString() = "PiImportChoice(<redacted>)"
}
internal data class PiSavedImport(
    val order: Long, val choice: PiImportChoice, val payload: String, val signature: String,
    val source: PiImportControlSource, val withdrawalRequested: Boolean,
    val supersededBy: String?, val outcome: PiImportOutcome?,
) {
    val resolved get() = supersededBy != null || outcome in setOf(PiImportOutcome.ACCEPTED, PiImportOutcome.EXPIRED, PiImportOutcome.WITHDRAWN)
    override fun toString() = "PiSavedImport(<redacted>)"
}

internal fun parsePiImports(json: JSONObject, selected: SelectedAppliance): PiImportsObservation {
    json.exactKeys("schemaVersion", "applianceId", "controllerId", "bondGeneration", "importsSupported", "importsAvailable", "context")
    require(json.strictLong("schemaVersion") == 1L && json.strictString("applianceId") == selected.appliance.deviceId &&
        json.strictString("controllerId") == selected.local.deviceId)
    val generation = json.strictString("bondGeneration").also(::requireCanonicalId)
    require(generation == selected.confirmedGeneration)
    val supported = json.strictBoolean("importsSupported"); val available = json.strictBoolean("importsAvailable")
    require((supported || !available) && available != json.isNull("context"))
    if (!available) return PiImportsObservation(generation, supported, null)
    val value = json.getJSONObject("context")
    val version = value.strictLong("schemaVersion"); require(version in 1L..2L)
    val fields = arrayOf("schemaVersion", "bond", "volumeServiceAvailable", "sourcesTruncated", "sources", "storage", "withdrawalAvailable", "sourceCleanupAvailable")
    value.exactKeys(*(if (version == 2L) fields + "cameraServiceAvailable" else fields))
    require(value.strictBoolean("withdrawalAvailable") && !value.strictBoolean("sourceCleanupAvailable"))
    val bond = parseImportBond(value.getJSONObject("bond"))
    require(bond.appliance == selected.appliance && bond.controller == selected.local && bond.nativeGeneration == generation)
    val volumes = value.strictBoolean("volumeServiceAvailable")
    val cameras = version == 2L && value.strictBoolean("cameraServiceAvailable")
    val space = if (value.isNull("storage")) null else value.getJSONObject("storage").let {
        it.exactKeys("availableAfterReserveBytes", "reserveBytes"); require(it.strictLong("reserveBytes") == 2_147_483_648L)
        it.strictLong("availableAfterReserveBytes").also { bytes -> require(bytes >= 0) }
    }
    val items = value.getJSONArray("sources"); require(items.length() <= 24)
    val sources = List(items.length()) { parseImportSource(items.getJSONObject(it), volumes, cameras, version == 2L) }
    require(sources.map { it.source.id }.toSet().size == sources.size && sources.groupBy { it.source.camera }.values.all { it.map { s -> s.session to s.sequence }.toSet().size <= 1 })
    sources.forEach { source -> if (source.confirmation != null) {
        require(source.source.connected && source.source.preview == PiPreviewState.READY && space != null && source.source.bytes!! <= space)
    } }
    return PiImportsObservation(generation, supported, PiImportContext(bond, volumes, value.strictBoolean("sourcesTruncated"), sources, space, if (version == 2L) cameras else null))
}

internal fun parseImportBond(value: JSONObject): PiImportBond {
    value.exactKeys("appliance", "controller", "rootGeneration", "nativeGeneration")
    return PiImportBond(value.getJSONObject("appliance").identity(), value.getJSONObject("controller").identity(),
        value.strictString("rootGeneration"), value.strictString("nativeGeneration"))
}
private fun parseImportSource(value: JSONObject, volumes: Boolean, cameras: Boolean = false, modern: Boolean = false): PiImportControlSource {
    val fields = arrayOf("sourceId", "kind", "name", "connected", "automatic", "destination", "preview", "actionSession", "actionSequence", "confirmation")
    value.exactKeys(*(if (modern) fields + "canRemember" else fields))
    require(modern || value.strictString("kind") == "volume")
    val source = parsePiImportSource(value, volumes, cameras)
    val canRemember = !modern || value.strictBoolean("canRemember")
    require((source.camera || canRemember) && (canRemember || !source.automatic))
    val session = value.strictString("actionSession").also(::requireCanonicalId)
    val sequence = value.strictLong("actionSequence").also { require(it > 0) }
    val confirmation = if (value.isNull("confirmation")) null else value.getJSONObject("confirmation").let {
        it.exactKeys("attachmentId", "previewId"); PiImportConfirmation(it.strictString("attachmentId"), it.strictString("previewId"))
    }
    require(confirmation == null || (source.connected && source.preview == PiPreviewState.READY))
    return PiImportControlSource(source, session, sequence, confirmation, canRemember)
}
private fun parseImportChoice(value: JSONObject): PiImportChoice {
    value.exactKeys("schemaVersion", "bond", "actionId", "actionSession", "actionSequence", "sourceId", "kind", "mode", "attachmentId", "previewId", "retention")
    val camera = value.strictString("kind") == "camera"
    require((camera && value.strictLong("schemaVersion") == 2L || !camera && value.strictString("kind") == "volume" && value.strictLong("schemaVersion") == 1L) && value.strictString("retention") == "retain")
    val mode = PiImportMode.entries.single { it.wire == value.strictString("mode") }
    val confirmation = if (mode == PiImportMode.OFF) {
        require(value.isNull("attachmentId") && value.isNull("previewId")); null
    } else PiImportConfirmation(value.strictString("attachmentId"), value.strictString("previewId"))
    return PiImportChoice(parseImportBond(value.getJSONObject("bond")), value.strictString("actionId"), value.strictString("actionSession"),
        value.strictLong("actionSequence"), value.strictString("sourceId"), mode, confirmation, camera)
}
private fun requireSignature(value: String) {
    val bytes = Base64.getUrlDecoder().decode(value)
    require(bytes.size == 64 && Base64.getUrlEncoder().withoutPadding().encodeToString(bytes) == value)
}
internal fun parsePiSavedImport(value: JSONObject, local: ApplianceIdentity): PiSavedImport {
    value.exactKeys("order", "proof", "source", "withdrawalRequested", "withdrawalSignature", "supersededBy", "receipt")
    val order = value.strictLong("order").also { require(it > 0) }
    val proof = value.getJSONObject("proof"); proof.exactKeys("payload", "signature")
    val payload = proof.strictString("payload").also { require(it.toByteArray(Charsets.UTF_8).size <= 2048) }
    val signature = proof.strictString("signature").also(::requireSignature)
    val choice = parseImportChoice(JSONObject(payload)); require(choice.bond.controller == local && payload == choice.canonicalPayload())
    val rawSource = value.getJSONObject("source")
    val source = parseImportSource(rawSource, true, true, rawSource.has("canRemember"))
    require(source.source.camera == choice.camera && source.source.id == choice.sourceId && source.session == choice.session && source.sequence == choice.sequence)
    require(choice.mode != PiImportMode.REMEMBER || source.canRemember)
    require(choice.mode == PiImportMode.OFF || source.confirmation == choice.confirmation)
    val withdrawn = value.strictBoolean("withdrawalRequested"); require(withdrawn != value.isNull("withdrawalSignature"))
    if (withdrawn) requireSignature(value.strictString("withdrawalSignature"))
    val superseded = if (value.isNull("supersededBy")) null else value.strictString("supersededBy").also(::requireCanonicalId)
    val outcome = if (value.isNull("receipt")) null else value.getJSONObject("receipt").let {
        it.exactKeys("actionId", "actionSession", "actionSequence", "outcome", "admissionOnly", "choiceDigest")
        require(it.strictString("actionId") == choice.id && it.strictBoolean("admissionOnly"))
        val digest = MessageDigest.getInstance("SHA-256").digest(payload.toByteArray(Charsets.UTF_8)).joinToString("") { byte -> "%02x".format(byte.toInt() and 255) }
        require(it.strictString("choiceDigest") == digest)
        PiImportOutcome.entries.single { outcome -> outcome.wire == it.strictString("outcome") }.also { outcome ->
            if (outcome == PiImportOutcome.UNKNOWN) require(it.isNull("actionSession") && it.isNull("actionSequence"))
            else require(it.strictString("actionSession") == choice.session && it.strictLong("actionSequence") == choice.sequence)
        }
    }
    return PiSavedImport(order, choice, payload, signature, source, withdrawn, superseded, outcome)
}
internal fun parsePiImportActions(value: JSONObject, local: ApplianceIdentity): List<PiSavedImport> {
    value.exactKeys("schemaVersion", "actions"); require(value.strictLong("schemaVersion") == 1L)
    val items = value.getJSONArray("actions"); require(items.length() <= 32)
    val actions = List(items.length()) { parsePiSavedImport(items.getJSONObject(it), local) }
    require(actions.map { it.order }.toSet().size == actions.size && actions.map { it.choice.id }.toSet().size == actions.size)
    actions.forEach { action -> action.supersededBy?.let { id ->
        val later = actions.single { it.choice.id == id }
        require(later.order > action.order && later.choice.mode == PiImportMode.OFF && later.outcome == PiImportOutcome.ACCEPTED &&
            later.choice.bond == action.choice.bond && later.choice.camera == action.choice.camera && later.choice.sourceId == action.choice.sourceId)
    } }
    return actions.sortedByDescending { it.order }
}

internal data class PiImportReview(val selected: SelectedAppliance, val context: PiImportContext, val source: PiImportControlSource, val pause: Boolean, val id: String = UUID.randomUUID().toString()) {
    fun choice(mode: PiImportMode): PiImportChoice {
        check(pause == (mode == PiImportMode.OFF))
        check(mode != PiImportMode.REMEMBER || source.canRemember)
        return PiImportChoice(context.bond, id, source.session, source.sequence, source.source.id, mode,
            if (mode == PiImportMode.OFF) null else checkNotNull(source.confirmation), source.source.camera)
    }
}
internal data class PiImportsState(
    val selected: SelectedAppliance? = null, val observation: PiImportsObservation? = null,
    val actions: List<PiSavedImport> = emptyList(), val actionsAvailable: Boolean = false,
) {
    val pending get() = actions.any { !it.resolved }
}
internal interface ApplianceImportsApi {
    fun pairing(): ApplianceContext
    fun context(selected: SelectedAppliance): PiImportsObservation
    fun actions(local: ApplianceIdentity): List<PiSavedImport>
    fun confirm(choice: PiImportChoice): PiSavedImport
    fun check(action: PiSavedImport): PiSavedImport
    fun withdraw(action: PiSavedImport): PiSavedImport
}

/** Re-read selection/pairing before writes. Lost replies are resolved only through saved requests. */
internal class ApplianceImportsController(private val store: ApplianceSelectionStore, private val api: ApplianceImportsApi) {
    private fun current(selected: SelectedAppliance) {
        check(store.read() == selected)
        val pairing = api.pairing()
        check(pairing.local == selected.local && selected.appliance in pairing.paired && store.read() == selected)
    }
    fun refresh(selected: SelectedAppliance, connected: Boolean): PiImportsState {
        current(selected)
        val actions = api.actions(selected.local).filter { it.choice.bond.appliance == selected.appliance }
        val observation = if (connected) api.context(selected) else null
        current(selected)
        return PiImportsState(selected, observation, actions, true)
    }
    fun review(selected: SelectedAppliance, sourceId: String, pause: Boolean): PiImportReview {
        current(selected)
        val context = checkNotNull(api.context(selected).context)
        val source = context.sources.single { it.source.id == sourceId }
        check(pause || (source.confirmation != null && api.actions(selected.local).none { it.choice.bond == context.bond && it.choice.camera == source.source.camera && !it.resolved }))
        current(selected)
        return PiImportReview(selected, context, source, pause)
    }
    fun confirm(review: PiImportReview, mode: PiImportMode): PiSavedImport {
        current(review.selected)
        val fresh = checkNotNull(api.context(review.selected).context)
        check(fresh.bond == review.context.bond && fresh.sources.single { it.source.id == review.source.source.id } == review.source)
        if (mode != PiImportMode.OFF) check(fresh.spaceAfterReserve != null && review.source.source.bytes!! <= fresh.spaceAfterReserve)
        current(review.selected)
        val result = api.confirm(review.choice(mode))
        current(review.selected)
        check(result.choice == review.choice(mode))
        return result
    }
    fun resolve(selected: SelectedAppliance, action: PiSavedImport, withdraw: Boolean): PiSavedImport {
        current(selected)
        check(action.choice.bond.appliance == selected.appliance && action.choice.bond.controller == selected.local)
        check(action.choice.bond.nativeGeneration == selected.confirmedGeneration)
        val result = if (withdraw) api.withdraw(action) else api.check(action)
        current(selected)
        check(result.choice == action.choice && result.source == action.source && result.payload == action.payload && result.signature == action.signature && result.order == action.order)
        return result
    }
}
