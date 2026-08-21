package life.michaelwong.covalent

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.lifecycle.SavedStateHandle
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.a11y.AccessibilityFinding
import life.michaelwong.covalent.a11y.ComposeAccessibilityAudit
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.PlatformTier
import life.michaelwong.covalent.ui.ConnectionHealth
import life.michaelwong.covalent.ui.CovalentApp
import life.michaelwong.covalent.ui.CovalentViewModel
import life.michaelwong.covalent.ui.Screen
import life.michaelwong.covalent.ui.theme.CovalentTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * The Android accessibility gate.
 *
 * Two halves, and both matter. [unlabelledActionIsCaught] and
 * [undersizedTouchTargetIsCaught] are negative controls: they build screens with known
 * defects and assert the audit reports them. Without those, a green run of the screen
 * tests below would prove only that the audit found nothing — including finding nothing
 * because it inspects nothing. The remaining tests assert real app screens are clean.
 */
class AccessibilityGateTest {
    @get:Rule
    val compose = createComposeRule()

    // ---- Negative controls ----------------------------------------------------------

    @Test
    fun unlabelledActionIsCaught() {
        var density = Density(1f)
        compose.setContent {
            density = LocalDensity.current
            CovalentTheme {
                Column {
                    // The defect: an icon-only button whose icon is marked decorative,
                    // which is exactly the mistake this gate exists to prevent.
                    IconButton(onClick = {}) {
                        Icon(Icons.Rounded.Settings, contentDescription = null)
                    }
                    // The control: the same button, labelled.
                    IconButton(onClick = {}) {
                        Icon(Icons.Rounded.Settings, contentDescription = "Settings")
                    }
                    Button(onClick = {}) { Text("Save") }
                }
            }
        }
        val result = audit(density, minimumInteractive = 3)
        val unlabelled = result.findings.filter {
            it.rule == AccessibilityFinding.Rule.UNLABELLED_ACTION
        }
        assertEquals(
            "The audit must report exactly the one unlabelled action: ${describe(result)}",
            1,
            unlabelled.size,
        )
    }

    @Test
    fun undersizedVisibleTargetIsCaught() {
        var density = Density(1f)
        compose.setContent {
            density = LocalDensity.current
            CovalentTheme {
                Column {
                    // The defect: a 16dp control, below the WCAG 2.2 AA minimum.
                    Box(Modifier.size(16.dp).semantics { onClick { true } }) { Text("Tiny") }
                    // Controls: a bare 24dp clickable sits exactly on the minimum, and a
                    // Material button is 40dp visible with the platform's 48dp touch
                    // expansion behind it. Neither may be reported.
                    Box(Modifier.size(24.dp).clickable {}) { Text("Exactly at the minimum") }
                    Button(onClick = {}) { Text("Big enough") }
                }
            }
        }
        val result = audit(density, minimumInteractive = 3)
        val undersized = result.findings.filter {
            it.rule == AccessibilityFinding.Rule.VISIBLE_TARGET_TOO_SMALL
        }
        assertEquals(
            "The audit must report exactly the one undersized target: ${describe(result)}",
            1,
            undersized.size,
        )
    }

    /**
     * Pins the platform behaviour the 48dp Android minimum touch target rests on.
     *
     * Compose expands `touchBoundsInRoot` to the minimum touch target unconditionally, so
     * every control in this app already satisfies the 48dp rule and a per-screen
     * assertion on it could never fail. That guarantee comes from the toolkit rather than
     * from this codebase, so it is asserted here directly: if a Compose upgrade stops
     * expanding, this test fails and the 48dp claim stops being true silently.
     */
    @Test
    fun composeExpandsSmallTargetsToTheAndroidMinimumTouchTarget() {
        var density = Density(1f)
        compose.setContent {
            density = LocalDensity.current
            CovalentTheme {
                Box(Modifier.size(16.dp).semantics { onClick { true } }) { Text("Tiny") }
            }
        }
        val node = compose.onRoot(useUnmergedTree = true).fetchSemanticsNode()
            .let(::firstInteractive)
        assertTrue("The probe control was not found in the semantics tree", node != null)
        val touch = requireNotNull(node).touchBoundsInRoot
        val minimum = with(density) { ComposeAccessibilityAudit.MINIMUM_TOUCH_TARGET.toPx() }
        assertTrue(
            "Compose no longer expands small targets to " +
                "${ComposeAccessibilityAudit.MINIMUM_TOUCH_TARGET}: a 16dp control reports " +
                "touch bounds of ${touch.width}x${touch.height}px against a ${minimum}px " +
                "minimum. The 48dp touch-target guarantee must now be enforced in app code.",
            touch.width >= minimum && touch.height >= minimum,
        )
        val visible = requireNotNull(node).size
        assertTrue(
            "The probe control was not laid out at the 16dp size this test depends on",
            visible.width < minimum,
        )
    }

    private fun firstInteractive(node: SemanticsNode): SemanticsNode? {
        if (node.config.contains(SemanticsActions.OnClick)) return node
        node.children.forEach { child -> firstInteractive(child)?.let { return it } }
        return null
    }

    @Test
    fun aCorrectlyBuiltScreenProducesNoFindings() {
        var density = Density(1f)
        compose.setContent {
            density = LocalDensity.current
            CovalentTheme {
                Column {
                    IconButton(onClick = {}) {
                        Icon(Icons.Rounded.Settings, contentDescription = "Settings")
                    }
                    Button(onClick = {}) { Text("Save") }
                    OutlinedTextField(
                        value = "",
                        onValueChange = {},
                        label = { Text("Server address") },
                    )
                }
            }
        }
        assertClean(density, "on a hand-built correct screen", minimumInteractive = 3)
    }

    // ---- Real screens ---------------------------------------------------------------

    @Test
    fun setupScreenIsAccessible() = assertAppScreenIsAccessible("a11y_setup", Screen.SETUP)

    @Test
    fun homeScreenIsAccessible() = assertAppScreenIsAccessible("a11y_home", Screen.HOME)

    @Test
    fun settingsScreenIsAccessible() = assertAppScreenIsAccessible("a11y_settings", Screen.SETTINGS)

    @Test
    fun pairScreenIsAccessible() = assertAppScreenIsAccessible("a11y_pair", Screen.PAIR)

    @Test
    fun backupScreenIsAccessible() = assertAppScreenIsAccessible("a11y_backup", Screen.BACKUP)

    @Test
    fun restoreScreenIsAccessible() = assertAppScreenIsAccessible("a11y_restore", Screen.RESTORE)

    // ---- Real screens at the largest text scale -------------------------------------
    //
    // Labels do not vanish at 2x, but layouts collapse and touch targets shrink, so the
    // default scale is not representative on its own. Each screen needs its own test:
    // the Compose rule accepts exactly one setContent per test.

    @Test
    fun homeScreenIsAccessibleAtLargestTextScale() =
        assertAppScreenIsAccessible("a11y_home_2x", Screen.HOME, LARGEST_FONT_SCALE)

    @Test
    fun settingsScreenIsAccessibleAtLargestTextScale() =
        assertAppScreenIsAccessible("a11y_settings_2x", Screen.SETTINGS, LARGEST_FONT_SCALE)

    @Test
    fun setupScreenIsAccessibleAtLargestTextScale() =
        assertAppScreenIsAccessible("a11y_setup_2x", Screen.SETUP, LARGEST_FONT_SCALE)

    @Test
    fun pairScreenIsAccessibleAtLargestTextScale() =
        assertAppScreenIsAccessible("a11y_pair_2x", Screen.PAIR, LARGEST_FONT_SCALE)

    private fun assertAppScreenIsAccessible(
        storeName: String,
        screen: Screen,
        fontScale: Float = 1f,
    ) {
        var density = Density(1f)
        val store = isolatedStore(storeName)
        val state = readyState(store, screen)
        compose.setContent {
            val scaled = Density(LocalDensity.current.density, fontScale)
            density = scaled
            CompositionLocalProvider(LocalDensity provides scaled) {
                CovalentTheme { CovalentApp(store, state) }
            }
        }
        assertClean(density, "on $screen at ${fontScale}x text", MINIMUM_SCREEN_CONTROLS)
    }

    private fun assertClean(density: Density, where: String, minimumInteractive: Int) {
        val result = audit(density, minimumInteractive)
        assertTrue(
            "Accessibility findings $where:\n${describe(result)}",
            result.findings.isEmpty(),
        )
    }

    /**
     * Runs the audit and refuses to return a result it could not have earned.
     *
     * The two guards are the point, in the spirit of the Apple Dynamic Type contract
     * test: an audit over an empty tree, or over a tree with no controls in it, reports
     * zero findings and looks green while proving nothing.
     */
    private fun audit(density: Density, minimumInteractive: Int): ComposeAccessibilityAudit.Result {
        val root: SemanticsNode = compose.onRoot(useUnmergedTree = true).fetchSemanticsNode()
        val result = ComposeAccessibilityAudit.audit(root, density)
        assertTrue(
            "The audit walked ${result.nodesVisited} nodes, fewer than the " +
                "$minimumInteractive controls this content must contain, so it cannot " +
                "have rendered and a clean result proves nothing.",
            result.nodesVisited >= minimumInteractive,
        )
        assertTrue(
            "The audit found only ${result.interactiveNodesVisited} interactive nodes, " +
                "fewer than the $minimumInteractive this screen must have, so a clean " +
                "result proves nothing.",
            result.interactiveNodesVisited >= minimumInteractive,
        )
        return result
    }

    private fun describe(result: ComposeAccessibilityAudit.Result): String =
        result.findings.joinToString("\n") { "  " + it.describe() }.ifBlank { "  (none)" } +
            "\n  (audited ${result.nodesVisited} nodes, " +
            "${result.interactiveNodesVisited} interactive)"

    private fun readyState(store: SecureNodeStore, screen: Screen): CovalentViewModel =
        CovalentViewModel(SavedStateHandle()).apply {
            initialize(store)
            this.screen = screen
            status = NodeStatus("Test node", 1u, false, PlatformTier.TIER_1, "ready")
            connectionHealth = ConnectionHealth.READY
        }

    private fun isolatedStore(suffix: String): SecureNodeStore {
        val base = InstrumentationRegistry.getInstrumentation().targetContext
        val isolatedContext = object : ContextWrapper(base) {
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                base.getSharedPreferences("instrumentation_${suffix}_$name", Context.MODE_PRIVATE)
        }
        isolatedContext.getSharedPreferences("covalent_node", Context.MODE_PRIVATE)
            .edit().clear().commit()
        return SecureNodeStore(isolatedContext)
    }

    private companion object {
        /** Android's largest user-selectable font scale. */
        const val LARGEST_FONT_SCALE = 2.0f

        /**
         * Every real app screen puts at least this many controls on screen. Falling
         * below it means the screen failed to render its content and the audit had
         * nothing to check.
         */
        const val MINIMUM_SCREEN_CONTROLS = 3
    }
}
