package life.michaelwong.covalent.node

import android.app.ForegroundServiceStartNotAllowedException
import android.content.ComponentName
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.os.Build
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test

class ProviderForegroundStartTest {
    @Test
    fun backgroundDenialCanRetryOnceVisible() {
        assumeTrue(Build.VERSION.SDK_INT >= 31)
        var background = true
        var attempts = 0
        val context = object : ContextWrapper(ApplicationProvider.getApplicationContext<Context>()) {
            override fun startForegroundService(service: Intent): ComponentName {
                attempts++
                if (background) throw ForegroundServiceStartNotAllowedException("test background denial")
                return ComponentName(this, NodeProviderService::class.java)
            }
        }
        val intent = Intent(context, NodeProviderService::class.java)
        assertFalse(requestProviderForegroundStart(context, intent))
        background = false
        assertTrue(requestProviderForegroundStart(context, intent))
        assertEquals(2, attempts)
    }

    @Test
    fun unexpectedStorageFailureStillPropagates() {
        val failure = IllegalStateException("test unexpected failure")
        val context = object : ContextWrapper(ApplicationProvider.getApplicationContext<Context>()) {
            override fun startForegroundService(service: Intent): ComponentName {
                throw failure
            }
        }
        try {
            requestProviderForegroundStart(context, Intent(context, NodeProviderService::class.java))
            throw AssertionError("Unrelated failures must not be reported as background restrictions")
        } catch (actual: IllegalStateException) {
            assertSame(failure, actual)
        }
    }
}
