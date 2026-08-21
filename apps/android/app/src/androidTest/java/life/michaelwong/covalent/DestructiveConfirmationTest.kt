package life.michaelwong.covalent

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.ui.DestructiveAction
import life.michaelwong.covalent.ui.DestructiveConfirmDialog
import life.michaelwong.covalent.ui.destructiveConfirmation
import life.michaelwong.covalent.ui.theme.CovalentTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

/**
 * Rendered behaviour of every destructive confirmation. The pure copy mapping is covered by
 * unit tests; what needs a device is that the dialog actually reaches the screen, that
 * confirming runs the destructive action exactly once, and that every way of backing out
 * leaves the action unrun.
 */
class DestructiveConfirmationTest {
    @get:Rule
    val compose = createComposeRule()

    private val resources = InstrumentationRegistry.getInstrumentation().targetContext.resources
    private val subject = "Documents"

    private var confirmed = 0
    private var dismissed = 0

    /**
     * Hosts one dialog whose action can be swapped between assertions, so a single
     * composition can walk every [DestructiveAction].
     */
    private fun hostSwitchableDialog(): (DestructiveAction) -> Unit {
        var action by mutableStateOf(DestructiveAction.entries.first())
        compose.setContent {
            CovalentTheme {
                DestructiveConfirmDialog(
                    action = action,
                    subject = subject,
                    onConfirm = { confirmed++ },
                    onDismiss = { dismissed++ },
                )
            }
        }
        return { next ->
            action = next
            confirmed = 0
            dismissed = 0
            compose.waitForIdle()
        }
    }

    @Test
    fun everyDestructiveActionRendersItsOwnNamedConfirmation() {
        val show = hostSwitchableDialog()
        val titlesSeen = mutableSetOf<String>()

        DestructiveAction.entries.forEach { action ->
            show(action)
            val copy = destructiveConfirmation(action)
            val title = resources.getString(copy.titleRes)
            val message = if (copy.namesSubject) {
                resources.getString(copy.messageRes, subject)
            } else {
                resources.getString(copy.messageRes)
            }

            compose.onNodeWithTag("confirm.${action.name}").assertIsDisplayed()
            compose.onNodeWithText(title).assertIsDisplayed()
            compose.onNodeWithText(message).assertIsDisplayed()
            compose.onNodeWithText(resources.getString(copy.confirmRes)).assertIsDisplayed()
            compose.onNodeWithText(resources.getString(copy.cancelRes)).assertIsDisplayed()

            // A confirmation that names the wrong thing is worse than none, so the subject
            // has to appear verbatim in the message whenever the action destroys a named thing.
            if (copy.namesSubject) {
                assertEquals(true, message.contains(subject))
            }
            titlesSeen += title
        }

        // Each action must be distinguishable on screen, not share one generic warning.
        assertEquals(DestructiveAction.entries.size, titlesSeen.size)
    }

    @Test
    fun confirmingADestructiveActionRunsItExactlyOnce() {
        val show = hostSwitchableDialog()

        DestructiveAction.entries.forEach { action ->
            show(action)
            compose.onNodeWithTag("confirm.${action.name}.proceed").performClick()
            compose.runOnIdle {
                assertEquals("$action should run once when confirmed", 1, confirmed)
                assertEquals("$action must not also report a dismissal", 0, dismissed)
            }
        }
    }

    @Test
    fun dismissingADestructiveActionNeverRunsIt() {
        val show = hostSwitchableDialog()

        DestructiveAction.entries.forEach { action ->
            show(action)
            compose.onNodeWithTag("confirm.${action.name}.cancel").performClick()
            compose.runOnIdle {
                assertEquals("$action must not run when dismissed", 0, confirmed)
                assertEquals("$action should report exactly one dismissal", 1, dismissed)
            }
        }
    }
}

