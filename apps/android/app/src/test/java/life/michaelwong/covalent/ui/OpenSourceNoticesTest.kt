package life.michaelwong.covalent.ui

import java.io.ByteArrayInputStream
import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class OpenSourceNoticesTest {
    @Test
    fun completeDescriptorAndBothExactAssetsAreRequired() {
        val notice = "Syncthing\nLicense text\n".toByteArray()
        val manifest = "{\"status\":\"texts-collected-review-required\"}\n".toByteArray()
        val index = index(notice, manifest)
        val assets = mutableMapOf(
            "sync-engine-notices-index.txt" to index.toByteArray(),
            "sync-engine-notices/THIRD-PARTY-NOTICES.txt" to notice,
            "sync-engine-notices/manifest.json" to manifest,
        )

        assertEquals("Syncthing\nLicense text\n", OpenSourceNotices.load(source(assets)))

        listOf(
            assets.toMutableMap().apply {
                this["sync-engine-notices/THIRD-PARTY-NOTICES.txt"] = notice + byteArrayOf(0)
            },
            assets.toMutableMap().apply {
                this["sync-engine-notices/manifest.json"] = manifest.copyOf(manifest.size - 1)
            },
            assets.toMutableMap().apply {
                this["sync-engine-notices/manifest.json"] = manifest.copyOf().also { it[0] = 'X'.code.toByte() }
            },
        ).forEach { invalid ->
            assertThrows(IllegalStateException::class.java) {
                OpenSourceNotices.load(source(invalid))
            }
        }
    }

    @Test
    fun hostileIndexesFailBeforeOpeningReferencedAssets() {
        val digest = "a".repeat(64)
        val valid = "1\n" +
            "combined $digest 12 sync-engine-notices/THIRD-PARTY-NOTICES.txt\n" +
            "manifest $digest 10 sync-engine-notices/manifest.json\n"
        val invalid = listOf(
            valid.dropLast(1),
            valid.replace("\n", "\r\n"),
            valid.replace("combined ", "combined  "),
            valid.replace("manifest", "combined"),
            valid.replace("a".repeat(64), "A".repeat(64)),
            valid.replace(" 12 ", " 012 "),
            valid.replace(" 12 ", " 0 "),
            valid.replace(" 12 ", " 8388609 "),
            valid.replace("THIRD-PARTY-NOTICES.txt", "other.txt"),
            valid + "extra\n",
        )

        invalid.forEach { value ->
            var opens = 0
            assertThrows(IllegalStateException::class.java) {
                OpenSourceNotices.load(NoticeAssetSource {
                    opens += 1
                    ByteArrayInputStream(value.toByteArray())
                })
            }
            assertEquals(1, opens)
        }
    }

    @Test
    fun descriptorAndNoticeTextMustUseStrictBoundedEncoding() {
        val manifest = "{}\n".toByteArray()
        val invalidUtf8 = byteArrayOf(0xc3.toByte(), 0x28)
        val assets = mapOf(
            "sync-engine-notices-index.txt" to index(invalidUtf8, manifest).toByteArray(),
            "sync-engine-notices/THIRD-PARTY-NOTICES.txt" to invalidUtf8,
            "sync-engine-notices/manifest.json" to manifest,
        )
        assertThrows(Exception::class.java) {
            OpenSourceNotices.load(source(assets))
        }

        val oversizedIndex = ByteArray(1_025) { 'x'.code.toByte() }
        assertThrows(IllegalStateException::class.java) {
            OpenSourceNotices.load(
                NoticeAssetSource { ByteArrayInputStream(oversizedIndex) },
            )
        }
    }

    private fun source(assets: Map<String, ByteArray>) = NoticeAssetSource { path ->
        ByteArrayInputStream(checkNotNull(assets[path]))
    }

    private fun index(notice: ByteArray, manifest: ByteArray): String =
        "1\n" +
            "combined ${sha256(notice)} ${notice.size} " +
            "sync-engine-notices/THIRD-PARTY-NOTICES.txt\n" +
            "manifest ${sha256(manifest)} ${manifest.size} " +
            "sync-engine-notices/manifest.json\n"

    private fun sha256(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256")
        .digest(bytes)
        .joinToString("") { "%02x".format(it) }
}
