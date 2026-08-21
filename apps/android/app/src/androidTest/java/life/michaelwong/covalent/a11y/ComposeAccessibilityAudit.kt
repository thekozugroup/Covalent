package life.michaelwong.covalent.a11y

import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp

/**
 * A single accessibility defect found in a Compose semantics tree.
 *
 * [describe] is written to be pasted into a bug: it names the rule, the offending node,
 * and enough of the surrounding tree to find it in the source.
 */
data class AccessibilityFinding(
    val rule: Rule,
    val nodeId: Int,
    val detail: String,
) {
    enum class Rule {
        /** An element a person can act on that a screen reader cannot name. */
        UNLABELLED_ACTION,

        /**
         * An element a person can act on whose visible target is under the WCAG 2.2 AA
         * minimum.  See [ComposeAccessibilityAudit.MINIMUM_VISIBLE_TARGET] for why this
         * is measured on the visible bounds rather than the touch bounds.
         */
        VISIBLE_TARGET_TOO_SMALL,
    }

    fun describe(): String = "[${rule.name}] node $nodeId: $detail"
}

/**
 * Static accessibility rules for a Compose screen.
 *
 * This exists because Espresso's `AccessibilityChecks` is a View-hierarchy checker and a
 * fully-Compose screen presents it with one opaque `AndroidComposeView`: it passes with
 * nothing inspected, which is worse than no gate at all. These rules run against the
 * semantics tree TalkBack actually consumes.
 *
 * The rules are deliberately narrow and mechanical. Each one is a defect no reasonable
 * screen should ever ship, so a finding is always a real bug rather than a judgement
 * call — which is what makes it safe to assert on zero findings in CI.
 */
object ComposeAccessibilityAudit {

    /**
     * Android's minimum touch target: the area in which a tap must land on the control.
     *
     * This is **not** what the size rule measures, and the reason is worth recording
     * because the obvious implementation is a gate that can never fail.  Compose reports
     * `SemanticsNode.touchBoundsInRoot` already padded out to this minimum, unconditionally
     * — measured on this project's Compose version, a 16dp box carrying nothing but a
     * `semantics { onClick { … } }` reports `bounds=16x16 touch=48x48`.  A rule asserting
     * `touchBoundsInRoot >= 48.dp` is therefore green by construction and proves nothing.
     *
     * The 48dp guarantee is real, but it comes from the platform rather than from this
     * codebase, so it is pinned by an explicit platform-contract test instead of by a
     * per-screen rule.  If a Compose upgrade ever stops expanding, that test fails.
     */
    val MINIMUM_TOUCH_TARGET = 48.dp

    /**
     * WCAG 2.2 AA (SC 2.5.8, Target Size Minimum), measured on the **visible** bounds.
     *
     * This is the part Compose does not hand out for free, so it is the part worth
     * gating: it fires on the 16dp control above and on the 24dp-or-smaller custom
     * targets that real screens accumulate, while leaving Material 3's own 40dp visible
     * controls — which pair a 40dp body with the 48dp touch target above, and are
     * conformant — alone.
     */
    val MINIMUM_VISIBLE_TARGET = 24.dp

    /**
     * Sub-pixel rounding can leave a nominally exact target a hair short once it has been
     * laid out and converted back to dp.  Half a dp of slack keeps the rule from firing
     * on arithmetic while staying far away from any target that is actually too small.
     */
    private val TARGET_TOLERANCE = 0.5.dp

    /**
     * Audits an **unmerged** semantics tree.
     *
     * Unmerged is required: a control's accessible name usually lives in a descendant
     * `Text`, and the merged tree hides the structure needed to tell "this button has a
     * label" from "this button is inside something that has one".
     */
    fun audit(root: SemanticsNode, density: Density): Result {
        val findings = mutableListOf<AccessibilityFinding>()
        val counters = Counters()
        visit(root, density, findings, counters)
        return Result(findings, counters.nodes, counters.interactive)
    }

    /**
     * The audit's findings plus what it actually looked at.
     *
     * [nodesVisited] and [interactiveNodesVisited] exist so a caller can prove the audit
     * inspected a real screen. Zero findings over zero nodes is not a pass.
     */
    data class Result(
        val findings: List<AccessibilityFinding>,
        val nodesVisited: Int,
        val interactiveNodesVisited: Int,
    )

    private class Counters {
        var nodes = 0
        var interactive = 0
    }

    private fun visit(
        node: SemanticsNode,
        density: Density,
        findings: MutableList<AccessibilityFinding>,
        counters: Counters,
    ) {
        counters.nodes++
        if (!isHidden(node)) {
            if (isInteractive(node)) {
                counters.interactive++
                if (accessibleName(node).isBlank()) {
                    findings += AccessibilityFinding(
                        AccessibilityFinding.Rule.UNLABELLED_ACTION,
                        node.id,
                        "acts on ${actionNames(node)} but nothing names it. " +
                            "Give it a contentDescription, a visible label, or a " +
                            "Modifier.semantics { contentDescription = … }. " +
                            "Nearest text: ${nearestText(node)}",
                    )
                }
                targetSizeFinding(node, density)?.let(findings::add)
            }
        }
        node.children.forEach { child -> visit(child, density, findings, counters) }
    }

    /**
     * Measures `SemanticsNode.size`, the unclipped layout size.
     *
     * Not `boundsInRoot`: that rect is intersected with the ancestors' clip, so a control
     * scrolled halfway past the edge of a `LazyColumn` reports a fraction of its real
     * height.  Measuring it produced exactly that false positive here — a full-height
     * `OutlinedButton` sitting at the bottom of the setup form reported `252x22dp` purely
     * because the viewport ended there.  Scroll position is not an accessibility defect.
     */
    private fun targetSizeFinding(node: SemanticsNode, density: Density): AccessibilityFinding? {
        val size = node.size
        // A zero-sized node was never laid out; it is not a small target.
        if (size.width <= 0 || size.height <= 0) return null
        val minimum = with(density) { (MINIMUM_VISIBLE_TARGET - TARGET_TOLERANCE).toPx() }
        if (size.width >= minimum && size.height >= minimum) return null
        val widthDp = with(density) { size.width.toDp() }
        val heightDp = with(density) { size.height.toDp() }
        return AccessibilityFinding(
            AccessibilityFinding.Rule.VISIBLE_TARGET_TOO_SMALL,
            node.id,
            "visible target is ${widthDp.value.toInt()}x${heightDp.value.toInt()}dp, " +
                "below the ${MINIMUM_VISIBLE_TARGET.value.toInt()}dp WCAG 2.2 AA minimum. " +
                "Add Modifier.minimumInteractiveComponentSize(), use a Material control, " +
                "or size the target up. Nearest text: ${nearestText(node)}",
        )
    }

    /** Elements a person can act on, and which therefore need a name and a real target. */
    private fun isInteractive(node: SemanticsNode): Boolean =
        INTERACTIVE_ACTIONS.any { key -> node.config.contains(key) }

    private fun actionNames(node: SemanticsNode): String =
        INTERACTIVE_ACTIONS.filter { node.config.contains(it) }
            .joinToString(", ") { it.name }
            .ifBlank { "an unknown action" }

    /**
     * Everything TalkBack would read out for this node: its own name, plus the names of
     * every descendant it merges or contains. A control labelled by a child `Text` is
     * correctly labelled, and an `Icon` marked `contentDescription = null` next to a
     * `Text` is correctly decorative.
     */
    private fun accessibleName(node: SemanticsNode): String = buildString {
        appendOwnName(node)
        node.children.forEach { child ->
            if (!isHidden(child)) append(' ').append(accessibleName(child))
        }
    }.trim()

    private fun StringBuilder.appendOwnName(node: SemanticsNode) {
        node.config.getOrNull(SemanticsProperties.ContentDescription)
            ?.let { append(' ').append(it.joinToString(" ")) }
        node.config.getOrNull(SemanticsProperties.Text)
            ?.let { append(' ').append(it.joinToString(" ") { text -> text.text }) }
        node.config.getOrNull(SemanticsProperties.EditableText)
            ?.let { append(' ').append(it.text) }
        node.config.getOrNull(SemanticsProperties.StateDescription)
            ?.let { append(' ').append(it) }
    }

    private fun isHidden(node: SemanticsNode): Boolean =
        node.config.getOrNull(SemanticsProperties.HideFromAccessibility) != null

    /** Best-effort locator so a failure message points at somewhere in the source. */
    private fun nearestText(node: SemanticsNode): String {
        var current: SemanticsNode? = node
        repeat(4) {
            val candidate = current ?: return "none"
            val name = buildString { appendOwnName(candidate) }.trim()
            if (name.isNotBlank()) return "\"${name.take(60)}\""
            val siblingText = candidate.parent
                ?.children
                ?.asSequence()
                ?.map { sibling -> buildString { appendOwnName(sibling) }.trim() }
                ?.firstOrNull(String::isNotBlank)
            if (!siblingText.isNullOrBlank()) return "\"${siblingText.take(60)}\""
            current = candidate.parent
        }
        return "none"
    }

    private val INTERACTIVE_ACTIONS = listOf(
        SemanticsActions.OnClick,
        SemanticsActions.OnLongClick,
        SemanticsActions.SetText,
        SemanticsActions.SetProgress,
    )
}
