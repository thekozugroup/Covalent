package life.michaelwong.covalent

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.pm.PackageManager
import android.content.Intent
import androidx.test.filters.SdkSuppress
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.runner.lifecycle.ActivityLifecycleMonitorRegistry
import androidx.test.runner.lifecycle.Stage
import java.util.concurrent.TimeUnit
import life.michaelwong.covalent.appliance.*
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.node.NodeProviderService
import org.junit.Assert.*
import org.junit.Before
import org.junit.After
import org.junit.Test

/** Normal Application/package; synthetic source data, never a real import/approval. */
@SdkSuppress(minSdkVersion = 33)
class InterlinkImportAlertsInstrumentedTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext

    // Isolate synthetic notices from the real observer; no identity or opt-in data is cleared.
    @Before fun stopHostBeforeNoticeFixture() = stopHost()
    @After fun stopHostAfterNoticeFixture() = stopHost()

    private fun stopHost() {
        val app = context.applicationContext as InterlinkApplication
        await { app.manager.value != null }
        val client = CovalentNodeClient(200)
        // Application reconnect can still be promising a foreground start. Settle it before a fixture stop.
        context.startActivity(context.packageManager.getLaunchIntentForPackage(context.packageName)!!)
        await { resumed().any { it is InterlinkActivity } }
        await {
            app.manager.value?.localConnectionForFolderSync()?.let {
                runCatching { client.applianceContext(it.baseUrl, it.token) }.isSuccess
            } == true
        }
        instrumentation.runOnMainSync { resumedOnMain().filterIsInstance<InterlinkActivity>().forEach { it.finish() } }
        await { resumed().isEmpty() }
        context.stopService(Intent(context, NodeProviderService::class.java))
        await {
            app.manager.value?.localConnectionForFolderSync()?.let {
                runCatching { client.applianceContext(it.baseUrl, it.token) }.isFailure
            } != false
        }
    }

    @Test fun actualNativeHostStartsOneObserverBeforeTheScreenCloses() {
        val app = context.applicationContext as InterlinkApplication
        val client = CovalentNodeClient(200)
        try {
            context.startActivity(context.packageManager.getLaunchIntentForPackage(context.packageName)!!)
            await {
                val connection = app.manager.value?.localConnectionForFolderSync()
                connection != null && runCatching { client.applianceContext(connection.baseUrl, connection.token) }.isSuccess &&
                    Thread.getAllStackTraces().keys.count { it.isAlive && it.name == "interlink-drive-alerts" } == 1
            }
        } finally {
            instrumentation.runOnMainSync { resumedOnMain().filterIsInstance<InterlinkActivity>().forEach { it.finish() } }
        }
        await { resumed().isEmpty() }
        assertEquals(1, Thread.getAllStackTraces().keys.count { it.isAlive && it.name == "interlink-drive-alerts" })
    }

    @Test fun notificationIsPrivateAndOpensReviewWithoutAnApprovalAction() {
        val sink = InterlinkImportNotifications(context)
        val n = sink.notification(alert())
        assertEquals(Notification.VISIBILITY_PRIVATE, n.visibility)
        assertTrue(n.flags and Notification.FLAG_AUTO_CANCEL != 0)
        assertTrue(n.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0)
        assertTrue(n.contentIntent.isImmutable)
        assertEquals(context.packageName, n.contentIntent.creatorPackage)
        assertTrue(n.actions.isNullOrEmpty())
        val publicText = n.publicVersion.extras.getString(Notification.EXTRA_TITLE)!!
        assertFalse(publicText.contains(SELECTED.name));assertFalse(publicText.contains("Camera SD card"))
        val detail = n.extras.getCharSequence(Notification.EXTRA_BIG_TEXT).toString()
        assertTrue(detail.contains("Originals stay on the source"));assertTrue(detail.contains(SELECTED.name))
        assertFalse(detail.contains(SELECTED.appliance.publicKey))
    }

    @Test fun backgroundLoopPostsWithoutResumingAnActivityAndStopRemovesIt() {
        val sink = InterlinkImportNotifications(context)
        assertEquals("Grant notification permission on the owned fixture before this journey", PackageManager.PERMISSION_GRANTED,
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS))
        assertTrue(sink.enabled());assertTrue(resumed().isEmpty())
        val loop = ApplianceImportAlertLoop({ alert() }, { SELECTED }, sink, 50);val owner = Any()
        try {
            loop.start(owner);await { sink.showing() }
            assertTrue("A reminder must not launch a screen", resumed().isEmpty())
            val active = context.getSystemService(NotificationManager::class.java).activeNotifications
                .single { it.id == InterlinkImportNotifications.ID && it.tag == InterlinkImportNotifications.TAG }
            assertEquals("Drive ready to review", active.notification.extras.getString(Notification.EXTRA_TITLE))
            loop.stop(owner);await { !sink.showing() }
        } finally { loop.close();sink.cancel() }
    }

    @Test fun notificationTapOpensTheRealInterlinkLauncher() {
        val sink = InterlinkImportNotifications(context)
        try {
            assertTrue(sink.post(alert(), false))
            sink.notification(alert()).contentIntent.send()
            await { resumed().any { it is InterlinkActivity } }
            assertEquals("life.michaelwong.interlink", context.packageName)
            assertTrue(context.applicationContext is InterlinkApplication)
        } finally {
            instrumentation.runOnMainSync { resumedOnMain().filterIsInstance<InterlinkActivity>().forEach { it.finish() } }
            sink.cancel()
        }
    }

    private fun resumedOnMain() = ActivityLifecycleMonitorRegistry.getInstance().getActivitiesInStage(Stage.RESUMED).toList()
    private fun resumed(): List<android.app.Activity> {
        var result = emptyList<android.app.Activity>()
        instrumentation.runOnMainSync { result = resumedOnMain() }
        return result
    }
    private fun await(condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!condition() && System.nanoTime() < deadline) Thread.sleep(20)
        assertTrue("Native background notification deadline expired", condition())
    }
    private fun alert(): ApplianceImportAlert {
        val example = interlinkImportExample(SELECTED)
        return checkNotNull(importAlert(PiImportsState(SELECTED,
            PiImportsObservation(GENERATION, true, example.context), emptyList(), true)))
    }
    private companion object {
        val PHONE = ApplianceIdentity("11111111-1111-4111-8111-111111111111", "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE", "a".repeat(20))
        val PI = ApplianceIdentity("22222222-2222-4222-8222-222222222222", "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI", "b".repeat(20))
        const val GENERATION = "44444444-4444-4444-8444-444444444444"
        val SELECTED = SelectedAppliance(PHONE, PI, "Example data · Living room Pi", GENERATION)
    }
}
