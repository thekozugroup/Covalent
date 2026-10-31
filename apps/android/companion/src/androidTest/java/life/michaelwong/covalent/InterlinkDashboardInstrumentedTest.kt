package life.michaelwong.covalent

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.appliance.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** Actual native rendering with synthetic identity/status; no accounts or real node. */
class InterlinkDashboardInstrumentedTest {
    @get:Rule val compose = createComposeRule()
    private val resources = InstrumentationRegistry.getInstrumentation().targetContext.resources

    @Test fun connectionLossRetainsSelectedPiWithoutOldRunningOrCloudClaims() {
        val state = mutableStateOf(connected())
        compose.setContent { InterlinkTheme { InterlinkApplianceCard(state.value, false, {}, {}) } }
        compose.onNodeWithTag("interlink-pi-name").assertTextEquals("Living room Pi")
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_connected)).assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_sync_running)).assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_cloud_unknown)).assertIsDisplayed()
        compose.runOnIdle { state.value = state.value.copy(connection = ApplianceConnectionState.UNAVAILABLE, observation = null) }
        compose.onNodeWithTag("interlink-pi-name").assertTextEquals("Living room Pi")
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_unavailable)).assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_sync_running)).assertDoesNotExist()
        compose.runOnIdle { state.value = connected().copy(observation = OBSERVATION.copy(syncState = ApplianceSyncState.UNAVAILABLE, shareRecords = null, healthFresh = false)) }
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_connected)).assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.interlink_pi_sync_unavailable)).assertIsDisplayed()
    }

    @Test fun approvalShowsBothFingerprintsAndConnectedStateHidesThem() {
        val state = mutableStateOf(connected().copy(connection = ApplianceConnectionState.APPROVAL_REQUIRED, observation = null))
        compose.setContent {
            InterlinkTheme {
                Column(Modifier.verticalScroll(rememberScrollState())) { InterlinkApplianceCard(state.value, false, {}, {}) }
            }
        }
        compose.onNodeWithTag("interlink-phone-fingerprint").performScrollTo()
            .assertTextEquals(resources.getString(R.string.interlink_pi_phone_fingerprint, PHONE.fingerprint))
        compose.onNodeWithTag("interlink-host-fingerprint").performScrollTo()
            .assertTextEquals(resources.getString(R.string.interlink_pi_host_fingerprint, PI.fingerprint))
        compose.runOnIdle { state.value = connected() }
        compose.onNodeWithTag("interlink-phone-fingerprint").assertDoesNotExist()
        compose.onNodeWithTag("interlink-host-fingerprint").assertDoesNotExist()
    }

    @Test fun largeTextKeepsDeviceNameAndBothActionsReachableAndRejectsDoubleSubmission() {
        val busy = mutableStateOf(false)
        var refreshes = 0;var choices = 0
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                InterlinkTheme {
                    Column(Modifier.width(280.dp).verticalScroll(rememberScrollState())) {
                        InterlinkApplianceCard(connected(), busy.value, { refreshes++ }, { choices++ })
                    }
                }
            }
        }
        compose.onNodeWithTag("interlink-pi-name").performScrollTo().assertIsDisplayed()
        val refresh = compose.onNodeWithTag("interlink-pi-refresh")
        val choose = compose.onNodeWithTag("interlink-pi-choose")
        refresh.performScrollTo().assertIsDisplayed().performClick()
        choose.performScrollTo().assertIsDisplayed().performClick()
        val refreshBounds = refresh.getUnclippedBoundsInRoot()
        val chooseBounds = choose.getUnclippedBoundsInRoot()
        assertTrue(chooseBounds.top >= refreshBounds.bottom)
        compose.runOnIdle { assertEquals(1, refreshes);assertEquals(1, choices);busy.value = true }
        refresh.assertIsNotEnabled();choose.assertIsNotEnabled()
    }

    private fun connected() = ApplianceDashboardState(SELECTED, ApplianceConnectionState.CONNECTED,
        ApplianceContext(PHONE, listOf(PI)), listOf(ApplianceCandidate(PI, SELECTED.name)), OBSERVATION)
    private companion object {
        val PHONE = ApplianceIdentity("11111111-1111-4111-8111-111111111111", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE", "a".repeat(20))
        val PI = ApplianceIdentity("22222222-2222-4222-8222-222222222222", "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI", "b".repeat(20))
        val SELECTED = SelectedAppliance(PHONE, PI, "Living room Pi", "44444444-4444-4444-8444-444444444444")
        val OBSERVATION = ApplianceObservation(SELECTED.confirmedGeneration!!, ApplianceSyncState.RUNNING, 1, true)
    }
}
