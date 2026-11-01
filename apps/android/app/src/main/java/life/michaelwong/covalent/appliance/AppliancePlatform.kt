package life.michaelwong.covalent.appliance

import org.json.JSONObject

internal data class AppliancePlatformObservation(val bondGeneration: String, val supported: Boolean, val snapshot: PiPlatformSnapshot?)
internal data class PiPlatformSnapshot(
    val volumesAvailable: Boolean, val camerasAvailable: Boolean, val truncated: Boolean,
    val sources: List<PiImportSource>, val spaceAfterReserve: Long?, val updates: PiUpdateState?,
)
internal data class PiUpdateState(val configured: Boolean, val automatic: Boolean, val busy: Boolean)
internal data class PiImportSource(
    val id: String, val camera: Boolean, val name: String, val connected: Boolean, val automatic: Boolean,
    val destination: String, val preview: PiPreviewState, val count: Long?, val bytes: Long?,
)
internal enum class PiPreviewState(val wire: String) {
    UNAVAILABLE("unavailable"), PENDING("pending"), SCANNING("scanning"), READY("ready"), INCOMPLETE("incomplete"),
}

internal fun parseAppliancePlatform(json: JSONObject, peerId: String, localId: String): AppliancePlatformObservation {
    json.exactKeys("schemaVersion", "applianceId", "controllerId", "bondGeneration", "platformSupported", "platformAvailable", "snapshot")
    require(json.strictLong("schemaVersion") == 1L)
    requireCanonicalId(peerId); requireCanonicalId(localId)
    require(json.strictString("applianceId") == peerId && json.strictString("controllerId") == localId)
    val generation = json.strictString("bondGeneration").also(::requireCanonicalId)
    val supported = json.strictBoolean("platformSupported")
    val available = json.strictBoolean("platformAvailable")
    require((supported || !available) && available != json.isNull("snapshot"))
    if (!available) return AppliancePlatformObservation(generation, supported, null)
    val value = json.getJSONObject("snapshot")
    value.exactKeys("schemaVersion", "volumeServiceAvailable", "cameraServiceAvailable", "sourcesTruncated", "sources", "storage", "updates", "importApprovalAvailable", "sourceCleanupAvailable")
    require(value.strictLong("schemaVersion") == 1L && !value.strictBoolean("importApprovalAvailable") && !value.strictBoolean("sourceCleanupAvailable"))
    val volumes = value.strictBoolean("volumeServiceAvailable"); val cameras = value.strictBoolean("cameraServiceAvailable")
    val items = value.getJSONArray("sources"); require(items.length() <= 24)
    val sources = List(items.length()) { index ->
        val source = items.getJSONObject(index)
        source.exactKeys("sourceId", "kind", "name", "connected", "automatic", "destination", "preview")
        val id = source.strictString("sourceId").also { require(it.matches(Regex("[0-9a-f]{64}"))) }
        val kind = source.strictString("kind").also { require(it in setOf("camera", "volume")) }
        val name = source.strictString("name").also { text ->
            require(text.isNotBlank() && text.codePointCount(0, text.length) <= 64 && text.codePoints().noneMatch {
                Character.isISOControl(it) || it in 0x200b..0x200f || it in 0x202a..0x202e || it in 0x2060..0x206f || it == 0xfeff
            })
        }
        val connected = source.strictBoolean("connected")
        require(!connected || if (kind == "camera") cameras else volumes)
        val destination = source.strictString("destination").also { require(it.matches(Regex("DCIM/GPhoto/Imports/[0-9a-f]{16}"))) }
        val preview = source.getJSONObject("preview"); preview.exactKeys("state", "counts")
        val state = PiPreviewState.entries.single { it.wire == preview.strictString("state") }
        require(connected || state == PiPreviewState.UNAVAILABLE)
        val counted = state in setOf(PiPreviewState.READY, PiPreviewState.INCOMPLETE)
        require(counted != preview.isNull("counts"))
        var count: Long? = null; var bytes: Long? = null
        if (counted) {
            val counts = preview.getJSONObject("counts"); counts.exactKeys("eligibleFiles", "eligibleBytes")
            count = counts.strictLong("eligibleFiles"); bytes = counts.strictLong("eligibleBytes")
            require(count in 0..100_000 && bytes >= count && bytes <= count * 4_294_967_296L)
        }
        PiImportSource(id, kind == "camera", name, connected, source.strictBoolean("automatic"), destination, state, count, bytes)
    }
    require(sources.map { it.id }.toSet().size == sources.size)
    val space = if (value.isNull("storage")) null else value.getJSONObject("storage").let {
        it.exactKeys("availableAfterReserveBytes", "reserveBytes"); require(it.strictLong("reserveBytes") == 2_147_483_648L)
        it.strictLong("availableAfterReserveBytes").also { bytes -> require(bytes >= 0) }
    }
    val updates = if (value.isNull("updates")) null else value.getJSONObject("updates").let {
        it.exactKeys("configured", "automatic", "busy")
        PiUpdateState(it.strictBoolean("configured"), it.strictBoolean("automatic"), it.strictBoolean("busy"))
    }
    return AppliancePlatformObservation(generation, supported,
        PiPlatformSnapshot(volumes, cameras, value.strictBoolean("sourcesTruncated"), sources, space, updates))
}
