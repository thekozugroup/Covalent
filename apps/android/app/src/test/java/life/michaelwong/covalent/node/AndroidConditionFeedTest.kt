package life.michaelwong.covalent.node

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidConditionFeedTest {
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
