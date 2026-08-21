package life.michaelwong.covalent

import life.michaelwong.covalent.model.PeerTransport
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.ProviderReachability
import life.michaelwong.covalent.data.peerTransportConnectPayload
import life.michaelwong.covalent.data.isSafeSafRestoreAction
import life.michaelwong.covalent.ui.isProviderEligibleForBackup
import life.michaelwong.covalent.ui.providerCapacityBytes
import life.michaelwong.covalent.ui.signedPeerTransport
import life.michaelwong.covalent.ui.validateSignedProviderBinding
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ProductUXContractTest {
    @Test
    fun exactSignedSha256ProviderPinIsAccepted() {
        val fingerprint = "a".repeat(64)
        val transport = PeerTransport(
            peerId = "peer-123",
            displayName = "Backup NAS",
            address = "100.100.100.10:8788",
            certificateDer = "signed-der",
            certificateFingerprint = fingerprint,
        )
        validateSignedProviderBinding(
            transport,
            Provider(
                peerId = transport.peerId,
                address = transport.address,
                fingerprint = fingerprint,
            ),
        )
    }

    @Test
    fun finalizedPairingUsesWholeSignedTransportObject() {
        val fingerprint = "e".repeat(64)
        val transport = signedPeerTransport(JSONObject().put(
            "peerTransport",
            JSONObject()
                .put("peerId", "peer-transport-1")
                .put("displayName", "Backup NAS")
                .put("address", "nas.example.ts.net:8788")
                .put("certificateDer", "signed-der")
                .put("certificateFingerprint", fingerprint),
        ))
        assertEquals("peer-transport-1", transport.peerId)
        assertEquals(fingerprint, transport.certificateFingerprint)
        val payload = peerTransportConnectPayload(transport)
        assertTrue(!payload.has("peerId"))
        assertEquals(transport.peerId, payload.getJSONObject("peerTransport").getString("peerId"))
    }

    @Test
    fun wrongOrNonCanonicalSignedPinIsRejected() {
        val transport = PeerTransport(
            peerId = "peer-123",
            displayName = "Backup NAS",
            address = "100.100.100.10:8788",
            certificateDer = "signed-der",
            certificateFingerprint = "b".repeat(64),
        )
        assertThrows(IllegalStateException::class.java) {
            validateSignedProviderBinding(
                transport,
                Provider(
                    peerId = transport.peerId,
                    address = transport.address,
                    fingerprint = "c".repeat(64),
                ),
            )
        }
        assertThrows(IllegalStateException::class.java) {
            validateSignedProviderBinding(
                transport.copy(certificateFingerprint = "B".repeat(64)),
                Provider(
                    peerId = transport.peerId,
                    address = transport.address,
                    fingerprint = "B".repeat(64),
                ),
            )
        }
    }

    @Test
    fun restoreOffersFailSkipAndRenameButNeverUnsafeReplace() {
        assertTrue(isSafeSafRestoreAction("file", "create_file"))
        assertTrue(isSafeSafRestoreAction("directory", "create_directory"))
        assertTrue(isSafeSafRestoreAction("directory", "keep_directory"))
        assertTrue(isSafeSafRestoreAction("file", "skip_file"))
        assertTrue(isSafeSafRestoreAction("file", "rename_file"))
        assertTrue(!isSafeSafRestoreAction("file", "replace_file"))
    }

    @Test
    fun replicaSelectionFailsClosedWithoutFreshReachableCapacity() {
        val base = Provider(
            peerId = "peer-capacity",
            address = "100.100.100.20:8788",
            fingerprint = "f".repeat(64),
        )
        assertTrue(!isProviderEligibleForBackup(base))
        assertTrue(!isProviderEligibleForBackup(base.copy(
            reachability = ProviderReachability.OFFLINE,
            capacityBytes = 1_024,
        )))
        assertTrue(isProviderEligibleForBackup(base.copy(
            reachability = ProviderReachability.CONNECTED,
            capacityBytes = 1_024,
        )))
    }

    @Test
    fun phoneProviderCapacityRequiresProtectedHeadroom() {
        assertEquals(
            2L * 1_073_741_824L to 512L * 1_024L * 1_024L,
            providerCapacityBytes("2", "0.5"),
        )
        assertEquals(null, providerCapacityBytes("0.49", "0"))
        assertEquals(null, providerCapacityBytes("2", "1.6"))
        assertEquals(null, providerCapacityBytes("NaN", "0.5"))
    }
}
