package life.michaelwong.covalent.node

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class PackagedSyncEngineTest {
    @Test
    fun appOwnedAndroidPrivateParentAllowsPlatform0771ButRejectsUnsafeMetadata() {
        val directory = 0x4000
        val regularFile = 0x8000
        val appUid = 10_234

        assertEquals(
            true,
            PackagedSyncEngine.privateRuntimeParentAllowed(directory or 0b111_111_001, appUid, appUid, appUid),
        )
        assertEquals(
            true,
            PackagedSyncEngine.privateRuntimeParentAllowed(directory or 0b111_000_000, appUid, appUid, appUid),
        )
        assertEquals(
            false,
            PackagedSyncEngine.privateRuntimeParentAllowed(directory or 0b111_111_001, appUid, appUid + 1, appUid),
        )
        assertEquals(
            false,
            PackagedSyncEngine.privateRuntimeParentAllowed(directory or 0b111_111_011, appUid, appUid, appUid),
        )
        assertEquals(
            false,
            PackagedSyncEngine.privateRuntimeParentAllowed(regularFile or 0b111_111_001, appUid, appUid, appUid),
        )

        assertEquals(
            true,
            PackagedSyncEngine.privateRuntimeChildAllowed(directory or 0b111_000_000, appUid, appUid),
        )
        assertEquals(
            false,
            PackagedSyncEngine.privateRuntimeChildAllowed(directory or 0b111_001_000, appUid, appUid),
        )
        assertEquals(
            false,
            PackagedSyncEngine.privateRuntimeChildAllowed(directory or 0b111_000_000, appUid + 1, appUid),
        )
    }

    @Test
    fun runtimeParentReservesTheCompletePrivateSocketSuffix() {
        assertEquals(true, PackagedSyncEngine.runtimeParentPathFits("a".repeat(74)))
        assertEquals(false, PackagedSyncEngine.runtimeParentPathFits("a".repeat(75)))
        assertEquals(false, PackagedSyncEngine.runtimeParentPathFits("é".repeat(38)))
    }

    @Test
    fun exactTwoAbiManifestParses() {
        val value = "arm64-v8a ${"a".repeat(64)} 30787168\n" +
            "x86_64 ${"b".repeat(64)} 32646984\n"

        val parsed = PackagedSyncEngine.parseHashManifest(value)

        assertEquals(setOf("arm64-v8a", "x86_64"), parsed.keys)
        assertEquals(30_787_168L, parsed.getValue("arm64-v8a").bytes)
        assertEquals("b".repeat(64), parsed.getValue("x86_64").sha256)
    }

    @Test
    fun malformedOrPartialManifestsFailClosed() {
        val valid = "arm64-v8a ${"a".repeat(64)} 30787168\n" +
            "x86_64 ${"b".repeat(64)} 32646984\n"
        val invalid = listOf(
            valid.dropLast(1),
            valid.replace("x86_64", "arm64-v8a"),
            valid.replace("a".repeat(64), "A".repeat(64)),
            valid.replace("30787168", "0"),
            valid.replace("32646984", "67108865"),
            valid.replace("30787168", "030787168"),
            valid.lines().take(2).reversed().joinToString("\n", postfix = "\n"),
            valid.replace(" ", "  "),
            valid.replace("\n", "\r\n"),
            valid + "extra\n",
        )

        invalid.forEach { value ->
            assertThrows(IllegalStateException::class.java) {
                PackagedSyncEngine.parseHashManifest(value)
            }
        }
    }
}
