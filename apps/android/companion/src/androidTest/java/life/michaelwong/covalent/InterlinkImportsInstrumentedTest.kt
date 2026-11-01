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

/** Native components and callbacks only; all pairing/import data is an explicit example. */
class InterlinkImportsInstrumentedTest {
    @get:Rule val compose = createComposeRule()
    private val resources = InstrumentationRegistry.getInstrumentation().targetContext.resources
    private val example = interlinkImportExample(SELECTED)

    @Test fun cameraWithoutSerialCanOnlyImportOnceAndShowsWhy() {
        val camera = interlinkImportExample(SELECTED, camera = true, canRemember = false)
        val choices = mutableListOf<PiImportMode>()
        compose.setContent { InterlinkTheme { InterlinkImportConfirmation(camera, false, {}, { choices.add(it) }) } }
        compose.onNodeWithTag("interlink-import-remember").assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.interlink_import_unstable_camera)).assertIsDisplayed()
        compose.onNodeWithTag("interlink-import-confirm").assertTextEquals(resources.getString(R.string.interlink_import_once)).performClick()
        compose.runOnIdle { assertEquals(listOf(PiImportMode.ONCE), choices) }
    }

    @Test fun largeTextCameraExplanationAndOriginalsStayReachableWithoutAnInactiveToggle() {
        val camera = interlinkImportExample(SELECTED, camera = true, canRemember = false)
        val choices = mutableListOf<PiImportMode>()
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                InterlinkTheme { Column(Modifier.width(280.dp)) { InterlinkImportConfirmation(camera, false, {}, { choices += it }) } }
            }
        }
        compose.onNodeWithTag("interlink-import-remember").assertDoesNotExist()
        compose.onNodeWithText(resources.getString(R.string.interlink_import_unstable_camera)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(resources.getString(R.string.interlink_import_originals)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag("interlink-import-confirm").assertIsDisplayed().assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(listOf(PiImportMode.ONCE), choices) }
    }

    @Test fun cameraControlsAppearOnceAndVolumePendingDoesNotBlockThem() {
        val camera = interlinkImportExample(SELECTED, camera = true)
        val volume = saved().copy(choice = saved().choice.copy(sourceId = "b".repeat(64)))
        val state = PiImportsState(SELECTED, PiImportsObservation(GENERATION, true, camera.context), listOf(volume), true)
        val dashboard = dashboard().copy(platform = AppliancePlatformObservation(GENERATION, true,
            PiPlatformSnapshot(true, true, false, listOf(camera.source.source), 8_500_000_000, null)))
        var reviews = 0
        compose.setContent { InterlinkTheme { Column(Modifier.verticalScroll(rememberScrollState())) { InterlinkPlatformCard(dashboard, state) { _, _ -> reviews++ } } } }
        compose.onAllNodesWithTag("interlink-source-${camera.source.source.id}").assertCountEquals(1)
        compose.onNodeWithTag("interlink-import-review-${camera.source.source.id}").performScrollTo().assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(1, reviews) }
        compose.onNodeWithText(resources.getString(R.string.interlink_camera_approval_unavailable)).assertDoesNotExist()
    }

    @Test fun newSourceDefaultsToOnceAndCannotSubmitTwice() {
        val choices = mutableListOf<PiImportMode>()
        compose.setContent { InterlinkTheme { InterlinkImportConfirmation(example, false, {}, { choices.add(it) }) } }
        compose.onNodeWithTag("interlink-import-remember").performScrollTo().assertIsOff()
        compose.onNodeWithTag("interlink-import-review-destination").assertDoesNotExist()
        compose.onNodeWithTag("interlink-import-folder-details").performScrollTo().performClick()
        compose.onNodeWithTag("interlink-import-review-destination").performScrollTo().assertTextEquals(example.source.source.destination)
        compose.onNodeWithTag("interlink-import-confirm").assertTextEquals(resources.getString(R.string.interlink_import_once)).performClick().performClick()
        compose.runOnIdle { assertEquals(listOf(PiImportMode.ONCE), choices) }
        compose.onNodeWithTag("interlink-import-confirm").assertIsNotEnabled()
    }

    @Test fun rememberRequiresExplicitChoiceAndBusyConfirmationCannotChangeIt() {
        val busy = mutableStateOf(false);val choices = mutableListOf<PiImportMode>()
        compose.setContent { InterlinkTheme { InterlinkImportConfirmation(example, busy.value, {}, { choices.add(it) }) } }
        val checkbox = compose.onNodeWithTag("interlink-import-remember")
        checkbox.performScrollTo().assertIsOff().performClick().assertIsOn()
        compose.runOnIdle { busy.value = true }
        checkbox.assertIsNotEnabled();compose.onNodeWithTag("interlink-import-confirm").assertIsNotEnabled()
        compose.runOnIdle { busy.value = false }
        compose.onNodeWithTag("interlink-import-confirm").performClick()
        compose.runOnIdle { assertEquals(listOf(PiImportMode.REMEMBER), choices) }
    }

    @Test fun pendingRequestBlocksNewImportAndOffersPauseWithoutCloudClaims() {
        val action = saved();val imports = PiImportsState(SELECTED, PiImportsObservation(GENERATION, true, example.context), listOf(action), true)
        var pauses = 0;var checks = 0
        compose.setContent { InterlinkTheme { Column(Modifier.verticalScroll(rememberScrollState())) {
            InterlinkPlatformCard(dashboard(), imports) { _, pause -> if (pause) pauses++ }
            InterlinkImportRequests(dashboard(), imports, false, { checks++ }, {})
        } } }
        compose.onNodeWithTag("interlink-import-review-${example.source.source.id}").performScrollTo().assertIsNotEnabled()
        compose.onNodeWithTag("interlink-import-pause-${example.source.source.id}").performScrollTo().assertIsEnabled().performClick()
        compose.onNodeWithTag("interlink-import-check-${action.choice.id}").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(1, pauses);assertEquals(1, checks) }
        compose.onNodeWithText(resources.getString(R.string.interlink_import_accepted)).assertDoesNotExist()
    }

    @Test fun offlineHistoryHasNoActiveControlsAndChangedSelectionHidesIt() {
        val action = saved();val state = mutableStateOf(dashboard().copy(connection = ApplianceConnectionState.UNAVAILABLE))
        val imports = PiImportsState(SELECTED, null, listOf(action), true)
        compose.setContent { InterlinkTheme { InterlinkImportRequests(state.value, imports, false, {}, {}) } }
        compose.onNodeWithTag("interlink-import-check-${action.choice.id}").assertIsNotEnabled()
        compose.onNodeWithText(resources.getString(R.string.interlink_import_history_offline)).assertIsDisplayed()
        compose.runOnIdle { state.value = state.value.copy(selection = SELECTED.copy(name = "New selection")) }
        compose.onNodeWithTag("interlink-import-requests").assertDoesNotExist()
    }

    @Test fun largeTextKeepsPreviewRememberAndConfirmationReachable() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 2f)) {
                InterlinkTheme { Column(Modifier.width(280.dp)) { InterlinkImportConfirmation(example, false, {}, {}) } }
            }
        }
        compose.onNodeWithTag("interlink-import-review-counts").performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag("interlink-import-destination").performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag("interlink-import-remember").performScrollTo().assertIsDisplayed().assertIsOff()
        compose.onNodeWithTag("interlink-import-confirm").assertIsDisplayed().assertIsEnabled()
    }

    private fun saved() = PiSavedImport(1, example.choice(PiImportMode.ONCE), example.choice(PiImportMode.ONCE).canonicalPayload(), "example-only", example.source, false, null, null)
    private fun dashboard() = ApplianceDashboardState(SELECTED, ApplianceConnectionState.CONNECTED, observation =
        ApplianceObservation(GENERATION, ApplianceSyncState.RUNNING, 1, true), platform = AppliancePlatformObservation(GENERATION, true,
            PiPlatformSnapshot(true, true, false, emptyList(), 8_500_000_000, PiUpdateState(true, true, false))))
    private companion object {
        val PHONE = ApplianceIdentity("11111111-1111-4111-8111-111111111111", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE", "a".repeat(20))
        val PI = ApplianceIdentity("22222222-2222-4222-8222-222222222222", "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI", "b".repeat(20))
        const val GENERATION = "44444444-4444-4444-8444-444444444444"
        val SELECTED = SelectedAppliance(PHONE, PI, "Living room Pi", GENERATION)
    }
}
