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
import life.michaelwong.covalent.appliance.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** Native UI callbacks only, with explicitly synthetic identities and policy. */
class InterlinkUpdatesInstrumentedTest {
    @get:Rule val compose = createComposeRule()
    private val phone=ApplianceIdentity("11111111-1111-4111-8111-111111111111","AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE","a".repeat(20))
    private val pi=ApplianceIdentity("22222222-2222-4222-8222-222222222222","AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI","b".repeat(20))
    private val selected=SelectedAppliance(phone,pi,"Example Pi","44444444-4444-4444-8444-444444444444")
    private fun dashboard()=ApplianceDashboardState(selected,ApplianceConnectionState.CONNECTED,
        observation=ApplianceObservation(selected.confirmedGeneration!!,ApplianceSyncState.RUNNING,1,true))

    @Test fun switchRequestsReviewAndWaitsForFreshConfirmedState() {
        val state=mutableStateOf(interlinkUpdateExample(selected));var requested:Boolean?=null
        compose.setContent {InterlinkTheme {InterlinkUpdatesCard(dashboard(),state.value,false,{requested=it},{})}}
        compose.onNodeWithTag("interlink-automatic-updates").assertIsOff().performClick().assertIsOff()
        compose.runOnIdle {assertEquals(true,requested);state.value=interlinkUpdateExample(selected,enabled=true)}
        compose.onNodeWithTag("interlink-automatic-updates").assertIsOn()
    }
    @Test fun offlineBusyUnconfiguredAndMissingHistoryCannotChangePolicy() {
        val board=mutableStateOf(dashboard());val state=mutableStateOf(interlinkUpdateExample(selected));var requests=0
        compose.setContent {InterlinkTheme {InterlinkUpdatesCard(board.value,state.value,false,{requests++},{})}}
        compose.runOnIdle {board.value=board.value.copy(connection=ApplianceConnectionState.UNAVAILABLE)}
        compose.onNodeWithTag("interlink-automatic-updates").assertDoesNotExist()
        compose.runOnIdle {board.value=dashboard();state.value=state.value.copy(observation=state.value.observation!!.copy(context=state.value.observation!!.context!!.copy(busy=true)))}
        compose.onNodeWithTag("interlink-automatic-updates").assertIsNotEnabled()
        compose.runOnIdle {state.value=interlinkUpdateExample(selected).copy(actionsAvailable=false)}
        compose.onNodeWithTag("interlink-automatic-updates").assertIsNotEnabled()
        compose.runOnIdle {val c=state.value.observation!!.context!!;state.value=state.value.copy(actionsAvailable=true,observation=state.value.observation!!.copy(context=c.copy(configured=false,enabled=false,revision=null)))}
        compose.onNodeWithTag("interlink-automatic-updates").assertDoesNotExist()
        compose.runOnIdle {assertEquals(0,requests)}
    }
    @Test fun largeTextKeepsBothUnconfirmedRecoveryActionsReachableWithoutAutoApplying() {
        var checks=0;var requested:Boolean?=null
        compose.setContent {
            val density=LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density,2f)) {
                InterlinkTheme {Column(Modifier.width(280.dp).verticalScroll(rememberScrollState())) {
                    InterlinkUpdatesCard(dashboard(),interlinkUpdateExample(selected,pending=true),false,{requested=it},{checks++})
                }}
            }
        }
        compose.runOnIdle {assertEquals(0,checks);assertNull(requested)}
        compose.onNodeWithTag("interlink-updates-check").performScrollTo().assertIsDisplayed().performClick()
        compose.onNodeWithTag("interlink-updates-keep").performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle {assertEquals(1,checks);assertEquals(false,requested)}
    }
    @Test fun largeTextConfirmationRequiresExplicitActionAndDismissesAfterOneSubmit() {
        val ctx=interlinkUpdateExample(selected).observation!!.context!!
        val review=PiUpdateReview(selected,ctx,PiUpdateChoice(ctx.bond,"99999999-9999-4999-8999-999999999999",ctx.revision!!,true))
        val shown=mutableStateOf(true);var submissions=0
        compose.setContent {
            val density=LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density,2f)) {
                InterlinkTheme {if(shown.value)InterlinkUpdateConfirmation(review,false,{shown.value=false},{submissions++;shown.value=false})}
            }
        }
        compose.runOnIdle {assertEquals(0,submissions)}
        compose.onNodeWithTag("interlink-updates-confirm").assertIsDisplayed().performClick()
        compose.onNodeWithTag("interlink-updates-confirm").assertDoesNotExist()
        compose.runOnIdle {assertEquals(1,submissions)}
    }
}
