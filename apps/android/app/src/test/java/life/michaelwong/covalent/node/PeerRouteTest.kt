package life.michaelwong.covalent.node

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PeerRouteTest {
    @Test fun vpnRouteStaysStableAcrossWifiAndCellular() {
        val vpn = PeerIpv4("tun0", "100.101.102.103")
        val cell = PeerIpv4("rmnet0", "100.70.1.2")
        assertEquals(vpn.address, preferredPeerRoute(listOf(vpn, cell, PeerIpv4("wlan0", "192.168.1.2"))))
        assertEquals(vpn.address, preferredPeerRoute(listOf(vpn, cell)))
        assertNull(preferredPeerRoute(listOf(cell)))
    }
    @Test fun localRouteChangeIsDetectedWithoutUsingLoopbackOrIpv6() {
        assertEquals("192.168.1.2", preferredPeerRoute(listOf(PeerIpv4("wlan0", "192.168.1.2"))))
        assertEquals("10.0.0.2", preferredPeerRoute(listOf(PeerIpv4("wlan0", "10.0.0.2"))))
        assertNull(preferredPeerRoute(listOf(PeerIpv4("lo", "127.0.0.1"), PeerIpv4("tun0", "fd7a::1"))))
    }
}
