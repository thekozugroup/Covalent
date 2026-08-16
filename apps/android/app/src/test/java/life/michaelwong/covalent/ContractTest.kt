package life.michaelwong.covalent

import life.michaelwong.covalent.model.PlatformTier
import life.michaelwong.covalent.model.PrimaryAction
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class ContractTest {
    @Test
    fun tierOneIsReleaseBlockingAndroidPolicy() {
        assertEquals("Tier 1", PlatformTier.TIER_1.label)
    }

    @Test
    fun primaryToolbarHasOnlyLockedScopeActions() {
        assertEquals(listOf("Pair", "Backup", "Restore"), PrimaryAction.entries.map { it.label })
        assertFalse(PrimaryAction.entries.any { it.label.contains("Sync") })
    }
}
