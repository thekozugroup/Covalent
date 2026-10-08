package life.michaelwong.covalent.node

import java.net.Inet4Address
import java.net.NetworkInterface

internal data class PeerIpv4(val interfaceName: String, val address: String)

/** Ignore carrier CGNAT; only a VPN interface identifies a Tailscale route. */
internal fun preferredPeerRoute(observed: List<PeerIpv4>): String? = observed.mapNotNull { candidate ->
    val octets = candidate.address.split('.').map { it.toIntOrNull() ?: return@mapNotNull null }
    if (octets.size != 4 || octets.any { it !in 0..255 }) return@mapNotNull null
    val tailnet = octets[0] == 100 && octets[1] in 64..127
    val privateLan = octets[0] == 10 || octets[0] == 192 && octets[1] == 168 ||
        octets[0] == 172 && octets[1] in 16..31
    if (tailnet && !candidate.interfaceName.startsWith("tun") || !tailnet && !privateLan) return@mapNotNull null
    val rank = if (tailnet) 0 else if (octets[0] == 172) 2 else 1
    Triple(rank, octets.fold(0L) { value, byte -> value * 256 + byte }, candidate.address)
}.minWithOrNull(compareBy({ it.first }, { it.second }))?.third

internal fun preferredPeerRoute(): String? = runCatching {
    val addresses = mutableListOf<PeerIpv4>()
    val interfaces = NetworkInterface.getNetworkInterfaces()
    while (interfaces.hasMoreElements()) {
        val network = interfaces.nextElement()
        if (!network.isUp || network.isLoopback) continue
        val entries = network.inetAddresses
        while (entries.hasMoreElements()) {
            val address = entries.nextElement()
            if (address is Inet4Address) address.hostAddress?.let { addresses += PeerIpv4(network.name, it) }
        }
    }
    preferredPeerRoute(addresses)
}.getOrNull()
