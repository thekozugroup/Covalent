package life.michaelwong.covalent.node

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidConditionFeedTest {
    @Test
    fun anyWifiNetworkCountsEvenWhenAnotherTransportCanBeDefault() {
        val state = AndroidConditionTransportState<String>()
        val token = state.start(initialWifi = listOf("local-wifi"), initialCharging = false)
        assertEquals(AndroidConditionSnapshot(true, false), state.snapshot(token))
        assertEquals(AndroidConditionSnapshot(true, false), state.updateWifi(token, "second-wifi", true))
        assertEquals(AndroidConditionSnapshot(true, false), state.updateWifi(token, "local-wifi", false))
        assertEquals(AndroidConditionSnapshot(false, false), state.updateWifi(token, "second-wifi", false))
    }

    @Test
    fun stoppedAndPreviousLifecycleCallbacksCannotChangeCurrentState() {
        val state = AndroidConditionTransportState<String>()
        val first = state.start(emptyList(), initialCharging = false)
        state.stop()
        assertNull(state.updateWifi(first, "late-wifi", true))
        assertNull(state.updateCharging(first, true))

        val second = state.start(emptyList(), initialCharging = false)
        assertNull(state.updateWifi(first, "old-wifi", true))
        assertEquals(AndroidConditionSnapshot(false, true), state.updateCharging(second, true))
        assertEquals(AndroidConditionSnapshot(true, true), state.updateWifi(second, "lan-wifi", true))
        assertEquals(AndroidConditionSnapshot(false, true), state.replaceWifi(second, emptyList()))
        assertNull(state.replaceWifi(first, listOf("stale-wifi")))
    }

    @Test
    fun reportsOnlyWhileActiveAndRequiredWithChangeAndFreshHeartbeat() {
        val state = AndroidConditionHeartbeat()
        val wifi = AndroidConditionSnapshot(wifiConnected = true, charging = false)
        val charging = wifi.copy(charging = true)

        assertFalse(state.shouldRefresh(0))
        state.start()
        assertTrue(state.shouldRefresh(0))
        state.refreshed(used = false, nowMs = 0)
        assertTrue(state.shouldReport(wifi, 0))
        state.reported(wifi, 0)
        assertFalse(state.shouldReport(wifi, 59_999))

        state.refreshed(used = true, nowMs = 1)
        assertFalse(state.shouldReport(wifi, 1))
        state.reported(wifi, 1)
        assertFalse(state.shouldReport(wifi, 59_999))
        assertTrue(state.shouldReport(charging, 2))
        state.reported(charging, 2)
        assertTrue(state.shouldReport(charging, 60_002))
        assertTrue(state.shouldRefresh(60_001))

        state.stop()
        assertFalse(state.shouldRefresh(120_000))
        assertFalse(state.shouldReport(wifi, 120_000))
    }
}
