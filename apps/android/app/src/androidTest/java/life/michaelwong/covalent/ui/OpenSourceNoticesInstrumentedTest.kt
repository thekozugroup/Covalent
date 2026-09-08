package life.michaelwong.covalent.ui

import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.BuildConfig
import org.junit.Assert.assertTrue
import org.junit.Test

class OpenSourceNoticesInstrumentedTest {
    @Test
    fun packagedNoticeBundleIsExactAndReadable() {
        assertTrue("The device release gate must package the maintained engine", BuildConfig.COVALENT_SYNC_ENGINE_PACKAGED)
        val text = OpenSourceNotices.load(
            InstrumentationRegistry.getInstrumentation().targetContext,
        )
        assertTrue(text.contains("github.com/syncthing/syncthing@"))
        assertTrue(text.contains("Go go1.26.7 / LICENSE"))
        assertTrue(text.contains("Covalent engine guardian / MIT license"))
        assertTrue(text.contains("Embedded Fork Awesome assets / OFL-1.1"))
    }
}
