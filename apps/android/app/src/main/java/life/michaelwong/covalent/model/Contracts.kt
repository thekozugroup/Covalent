package life.michaelwong.covalent.model

enum class PlatformTier(val label: String) {
    TIER_1("Tier 1"),
    TIER_2("Tier 2"),
}

data class NodeStatus(
    val deviceName: String,
    val protocolVersion: UShort,
    val lanDiscovery: Boolean,
    val platformTier: PlatformTier,
    val state: String,
)

enum class PrimaryAction(val label: String) {
    PAIR("Pair"),
    BACKUP("Backup"),
    RESTORE("Restore"),
}
