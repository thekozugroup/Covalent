package life.michaelwong.covalent.sync

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Test

class PeerAddressUpdateTest {
    @Test
    fun captureRetainsTheExactRequestAcrossRetryAndEditingInvalidatesIt() {
        val draft = PeerAddressUpdateDraft(
            PEER,
            "Kitchen computer",
            "192.0.2.10:8787",
        ).edit("[2001:db8::20]:8789")!!.capture()
        val expected = PeerAddressRefreshRequest(
            PEER,
            "192.0.2.10:8787",
            "[2001:db8::20]:8789",
        )

        assertEquals(expected, draft.pendingRequest)
        assertSame(draft, draft.capture())
        assertSame(draft, draft.afterFailure("peer_address_unreachable"))
        assertSame(draft, draft.afterFailure("folder_sync_busy"))
        assertSame(draft, draft.afterStatus(PEER, "192.0.2.10:8787"))
        assertSame(draft, draft.afterStatus(PEER, "[2001:db8::20]:8789"))
        assertNull(draft.afterStatus(PEER, "192.0.2.99:8787"))
        assertNull(draft.afterStatus("33333333-3333-4333-8333-333333333333", "192.0.2.10:8787"))
        assertNull(draft.afterStatus(null, null))

        val edited = draft.edit("192.0.2.30:8790")!!
        assertEquals("192.0.2.30:8790", edited.candidateAddress)
        assertNull(edited.pendingRequest)
        assertNull(draft.afterFailure("peer_address_changed"))
    }

    @Test
    fun captureRejectsSameAddressAndNeverSilentlyNormalizesInput() {
        val same = PeerAddressUpdateDraft(PEER, "Peer", "192.0.2.10:8787")
            .edit("192.0.2.10:8787")!!
        assertThrows(IllegalArgumentException::class.java) { same.capture() }

        for (candidate in listOf(" 192.0.2.11:8787", "192.0.2.11:8787 ")) {
            val draft = PeerAddressUpdateDraft(PEER, "Peer", "192.0.2.10:8787").edit(candidate)!!
            assertThrows(IllegalArgumentException::class.java) { draft.capture() }
        }
        val clean = PeerAddressUpdateDraft(PEER, "Peer", "192.0.2.10:8787")
        assertNull(clean.edit("1".repeat(MAX_PEER_ADDRESS_CHARS + 1)))
        assertNull(clean.edit("192.0.2.11:8787\n"))
    }

    @Test
    fun numericValidationRejectsHostnamesAmbiguousPortsAndControls() {
        assertEquals("192.0.2.10:8787", requireNumericPeerAddress("192.0.2.10:8787"))
        assertEquals("[2001:db8::20]:8787", requireNumericPeerAddress("[2001:db8::20]:8787"))
        assertEquals(
            "[::ffff:192.0.2.20]:8787",
            requireNumericPeerAddress("[::ffff:192.0.2.20]:8787"),
        )

        for (candidate in listOf(
            "",
            "peer.example:8787",
            "192.0.2.10",
            "192.0.2.010:8787",
            "192.0.2.256:8787",
            "192.0.2.10:0",
            "192.0.2.10:08787",
            "0.0.0.0:8787",
            "224.0.0.1:8787",
            "255.255.255.255:8787",
            "[::]:8787",
            "[ff02::1]:8787",
            "[fe80::1]:8787",
            "[fe80::1%2]:8787",
            "[2001:db8::20]8787",
            "192.0.2.10:8787\n",
        )) {
            assertThrows(IllegalArgumentException::class.java) {
                requireNumericPeerAddress(candidate)
            }
        }
    }

    @Test
    fun awaitedFollowUpClearsStaleStateOnConflictAndSeparatesRefreshFailures() = runBlocking {
        val captured = PeerAddressUpdateDraft(PEER, "Peer", "192.0.2.10:8787")
            .edit("192.0.2.20:8787")!!
            .capture()
        val events = mutableListOf<String>()
        val changed = completePeerAddressUpdate(
            captured,
            submit = {
                events += "submit:${it.expectedAddress}->${it.candidateAddress}"
                throw AddressFailure("peer_address_changed")
            },
            reloadStatus = { events += "reload"; "fresh-address-row" },
            refreshProviders = { events += "providers" },
            errorCode = { (it as? AddressFailure)?.code },
        )
        assertEquals(
            listOf("submit:192.0.2.10:8787->192.0.2.20:8787", "reload"),
            events,
        )
        assertEquals("fresh-address-row", (changed as PeerAddressUpdateOutcome.Changed<*>).freshStatus)

        events.clear()
        val updated = completePeerAddressUpdate(
            captured,
            submit = { events += "submit" },
            reloadStatus = { events += "reload"; error("status unavailable") },
            refreshProviders = { events += "providers"; error("provider unavailable") },
            errorCode = { null },
        ) as PeerAddressUpdateOutcome.Updated<*>
        assertEquals(listOf("submit", "reload", "providers"), events)
        assertNull(updated.freshStatus)
        assertEquals("status unavailable", updated.statusFailure?.message)
        assertEquals("provider unavailable", updated.providerFailure?.message)

        val providerOnly = completePeerAddressUpdate(
            captured,
            submit = {},
            reloadStatus = { "fresh-status" },
            refreshProviders = { error("provider unavailable") },
            errorCode = { null },
        ) as PeerAddressUpdateOutcome.Updated<*>
        assertEquals("fresh-status", providerOnly.freshStatus)
        assertNull(providerOnly.statusFailure)
        assertEquals("provider unavailable", providerOnly.providerFailure?.message)

        var changedProvidersCalled = false
        val changedWithoutStatus = completePeerAddressUpdate(
            captured,
            submit = { throw AddressFailure("peer_address_changed") },
            reloadStatus = { error("status unavailable") },
            refreshProviders = { changedProvidersCalled = true },
            errorCode = { (it as? AddressFailure)?.code },
        ) as PeerAddressUpdateOutcome.Changed<*>
        assertNull(changedWithoutStatus.freshStatus)
        assertEquals(false, changedProvidersCalled)
    }

    @Test
    fun ambiguousFailureRetainsExactRequestWithoutRunningFollowUp() = runBlocking {
        val captured = PeerAddressUpdateDraft(PEER, "Peer", "192.0.2.10:8787")
            .edit("192.0.2.20:8787")!!
            .capture()
        var followUpCalled = false
        val outcome = completePeerAddressUpdate(
            captured,
            submit = { throw AddressFailure("peer_address_unreachable") },
            reloadStatus = { followUpCalled = true },
            refreshProviders = { followUpCalled = true },
            errorCode = { (it as? AddressFailure)?.code },
        ) as PeerAddressUpdateOutcome.Failed
        assertSame(captured, outcome.draft)
        assertEquals("peer_address_unreachable", (outcome.failure as AddressFailure).code)
        assertEquals(false, followUpCalled)
    }

    private class AddressFailure(val code: String) : Exception()

    private companion object {
        const val PEER = "22222222-2222-4222-8222-222222222222"
    }
}
