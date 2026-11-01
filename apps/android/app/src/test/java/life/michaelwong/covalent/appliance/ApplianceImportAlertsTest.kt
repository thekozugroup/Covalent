package life.michaelwong.covalent.appliance

import java.net.SocketTimeoutException
import java.util.UUID
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import life.michaelwong.covalent.data.CovalentNodeClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class ApplianceImportAlertsTest {
    @Test fun cameraCanAlertWithVolumeServiceUnavailableAndIndependentVolumePending() {
        val state = state();val observation = state.observation!!;val context = observation.context!!
        val camera = context.sources.single().copy(source = context.sources.single().source.copy(camera = true), canRemember = false)
        val modern = state.copy(observation = observation.copy(context = context.copy(volumesAvailable = false, camerasAvailable = true, sources = listOf(camera))))
        assertNotNull(importAlert(modern));assertNotNull(importAlert(modern.copy(actions = listOf(saved()))))
        val pendingCamera = saved().copy(choice = saved().choice.copy(camera = true), source = camera)
        assertNull(importAlert(modern.copy(actions = listOf(pendingCamera))))
        assertNull(importAlert(modern.copy(observation = observation.copy(context = modern.observation!!.context!!.copy(camerasAvailable = false)))))
    }
    @Test fun alertContainsBoundedCountsButNoCommandAndIgnoresNonceChurn() {
        val state = state();val alert = checkNotNull(importAlert(state))
        assertEquals(5L, alert.bytes);assertEquals(state.observation!!.context!!.sources.single().source.count, alert.files)
        assertTrue(alert.key.matches(Regex("[0-9a-f]{64}")))
        val context = state.observation.context!!
        assertEquals(alert.key, importAlert(state.copy(observation = state.observation.copy(context = context.copy(
            sources = context.sources.map { it.copy(sequence = it.sequence + 1) }))))!!.key)
        val changed = context.sources.single().copy(confirmation = context.sources.single().confirmation!!.copy(previewId = UUID.randomUUID().toString()))
        assertNotEquals(alert.key, importAlert(state.copy(observation = state.observation.copy(context = context.copy(sources = listOf(changed)))))!!.key)
    }

    @Test fun missingAuthorityGenerationSpaceOrSourcesNeverAlerts() {
        val s = state();val o = s.observation!!;val c = o.context!!
        val invalid = listOf(s.copy(actionsAvailable = false), s.copy(selected = null), s.copy(observation = null),
            s.copy(observation = o.copy(supported = false)), s.copy(observation = o.copy(generation = UUID.randomUUID().toString())),
            s.copy(observation = o.copy(context = c.copy(volumesAvailable = false))),
            s.copy(observation = o.copy(context = c.copy(spaceAfterReserve = null))),
            s.copy(observation = o.copy(context = c.copy(sources = emptyList()))),
            s.copy(observation = o.copy(context = c.copy(bond = c.bond.copy(nativeGeneration = UUID.randomUUID().toString())))))
        invalid.forEach { assertNull(importAlert(it)) }
        val source = c.sources.single()
        val unavailable = listOf(source.copy(confirmation = null), source.copy(source = source.source.copy(automatic = true)),
            source.copy(source = source.source.copy(connected = false)), source.copy(source = source.source.copy(camera = true)),
            source.copy(source = source.source.copy(preview = PiPreviewState.INCOMPLETE)), source.copy(source = source.source.copy(count = 0)),
            source.copy(source = source.source.copy(bytes = c.spaceAfterReserve!! + 1)))
        unavailable.forEach { assertNull(importAlert(s.copy(observation = o.copy(context = c.copy(sources = listOf(it)))))) }
    }

    @Test fun unresolvedCurrentBondBlocksAndAcceptedPreviewDoesNotPromptAgain() {
        val state = state();val pending = saved()
        assertNull(importAlert(state.copy(actions = listOf(pending))))
        assertNull(importAlert(state.copy(actions = listOf(pending.copy(outcome = PiImportOutcome.ACCEPTED)))))
        val old = pending.copy(choice = pending.choice.copy(bond = pending.choice.bond.copy(rootGeneration = UUID.randomUUID().toString())))
        assertNotNull(importAlert(state.copy(actions = listOf(old))))
        val different = pending.copy(choice = pending.choice.copy(confirmation = pending.choice.confirmation!!.copy(previewId = UUID.randomUUID().toString())),
            outcome = PiImportOutcome.ACCEPTED)
        assertNotNull(importAlert(state.copy(actions = listOf(different))))
    }

    @Test fun combinedCountsNeverWrapAndSourceOrderDoesNotChangeAlertIdentity() {
        val s = state();val o = s.observation!!;val c = o.context!!;val first = c.sources.single()
        val second = first.copy(source = first.source.copy(id = "f".repeat(64)))
        fun withSources(sources: List<PiImportControlSource>) = s.copy(observation = o.copy(context = c.copy(sources = sources)))
        assertEquals(importAlert(withSources(listOf(first, second)))!!.key, importAlert(withSources(listOf(second, first)))!!.key)
        val huge = first.copy(source = first.source.copy(count = Long.MAX_VALUE))
        assertThrows(ArithmeticException::class.java) { importAlert(withSources(listOf(huge, second))) }
    }

    @Test fun withdrawalAndAcceptedPauseSuppressOnlyTheirOldPreview() {
        val s = state();val action = saved()
        assertNull(importAlert(s.copy(actions = listOf(action.copy(outcome = PiImportOutcome.WITHDRAWN)))))
        val pause = action.copy(choice = action.choice.copy(mode = PiImportMode.OFF, confirmation = null), outcome = PiImportOutcome.ACCEPTED)
        assertNull(importAlert(s.copy(actions = listOf(pause))))
        val c = s.observation!!.context!!
        val fresh = c.sources.single().copy(confirmation = c.sources.single().confirmation!!.copy(previewId = UUID.randomUUID().toString()))
        assertNotNull(importAlert(s.copy(actions = listOf(pause), observation = s.observation.copy(context = c.copy(sources = listOf(fresh))))))
    }

    @Test fun lateOldOwnerCannotPublishOrStopTheNewOwner() {
        val entered = CountDownLatch(1);val release = CountDownLatch(1);val reads = AtomicInteger();val sink = Sink();val first = Any();val next = Any()
        val old = checkNotNull(importAlert(state()));val fresh = old.copy(key = "f".repeat(64))
        val loop = ApplianceImportAlertLoop({ if (reads.incrementAndGet() == 1) { entered.countDown();release.await(3, TimeUnit.SECONDS);old } else fresh },
            { SELECTED }, sink, 10)
        try {
            loop.start(first);assertTrue(entered.await(2, TimeUnit.SECONDS));loop.start(next);loop.stop(first);release.countDown()
            await { sink.posts.isNotEmpty() };assertTrue(sink.posts.all { it.first.key == fresh.key })
            loop.stop(first);val n = reads.get();await { reads.get() > n }
        } finally { release.countDown();loop.close() }
    }

    @Test fun changedSelectionAndStopDiscardInFlightReads() {
        val entered = CountDownLatch(1);val release = CountDownLatch(1);val sink = Sink();val selected = AtomicReference<SelectedAppliance?>(SELECTED)
        val loop = ApplianceImportAlertLoop({ entered.countDown();release.await(3, TimeUnit.SECONDS);importAlert(state()) }, selected::get, sink, 10)
        val owner = Any()
        try {
            loop.start(owner);assertTrue(entered.await(2, TimeUnit.SECONDS));selected.set(SELECTED.copy(name = "Other Pi"));release.countDown()
            await { sink.cancellations.get() > 0 };assertTrue(sink.posts.isEmpty())
            loop.stop(owner);selected.set(SELECTED);val cancelled = sink.cancellations.get()
            Thread.sleep(40);assertEquals(cancelled, sink.cancellations.get());assertTrue(sink.posts.isEmpty())
        } finally { release.countDown();loop.close() }
    }

    @Test fun restartingTheSameServiceOwnerRejectsItsEarlierRead() {
        val entered = CountDownLatch(1);val release = CountDownLatch(1);val calls = AtomicInteger();val sink = Sink();val owner = Any()
        val old = checkNotNull(importAlert(state()));val fresh = old.copy(key = "b".repeat(64))
        val loop = ApplianceImportAlertLoop({ if (calls.incrementAndGet() == 1) { entered.countDown();release.await(3, TimeUnit.SECONDS);old } else fresh },
            { SELECTED }, sink, 10)
        try {
            loop.start(owner);assertTrue(entered.await(2, TimeUnit.SECONDS));loop.stop(owner);loop.start(owner);release.countDown()
            await { sink.posts.isNotEmpty() };assertTrue(sink.posts.all { it.first.key == fresh.key })
        } finally { release.countDown();loop.close() }
    }

    @Test fun deniedPermissionDoesNotReadAndDismissalDoesNotRepostEveryPoll() {
        val reads = AtomicInteger();val sink = Sink().apply { allowed.set(false) };val value = AtomicReference(checkNotNull(importAlert(state())))
        val loop = ApplianceImportAlertLoop({ reads.incrementAndGet();value.get() }, { SELECTED }, sink, 10)
        try {
            loop.start(Any());await { sink.cancellations.get() > 1 };assertEquals(0, reads.get())
            sink.allowed.set(true);await { sink.posts.isNotEmpty() }
            val count = synchronized(loop) { sink.active.set(false);sink.posts.size };val before = reads.get()
            await { reads.get() >= before + 3 };assertEquals(count, sink.posts.size)
            value.set(value.get().copy(key = "e".repeat(64)));await { sink.posts.size > count }
        } finally { loop.close() }
    }

    @Test fun transientFailureHidesStaleDetailsAndRestoresSilently() {
        val value = AtomicReference<ApplianceImportAlert?>(importAlert(state()));val sink = Sink()
        val loop = ApplianceImportAlertLoop(value::get, { SELECTED }, sink, 10)
        try {
            loop.start(Any());await { sink.posts.isNotEmpty() };assertFalse(sink.posts.first().second)
            value.set(null);await { !sink.active.get() };val count = sink.posts.size
            value.set(importAlert(state()));await { sink.posts.size > count };assertTrue(sink.posts.last().second)
        } finally { loop.close() }
    }

    @Test fun notificationFailureCannotKillThePollingWorker() {
        val sink = Sink().apply { failNext.set(true) }
        val loop = ApplianceImportAlertLoop({ importAlert(state()) }, { SELECTED }, sink, 10)
        try { loop.start(Any());await { sink.posts.isNotEmpty() };assertFalse(sink.failNext.get()) }
        finally { loop.close() }
    }

    @Test fun boundedClientTimesOutAndCanReadTheNextAuthenticatedReply() {
        assertThrows(IllegalArgumentException::class.java) { CovalentNodeClient(0) }
        assertThrows(IllegalArgumentException::class.java) { CovalentNodeClient(30_001) }
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(envelope().toString()).setHeadersDelay(300, TimeUnit.MILLISECONDS))
            server.enqueue(MockResponse().setBody(envelope().toString()))
            val client = CovalentNodeClient(100);val url = server.url("/").toString().removeSuffix("/")
            assertThrows(SocketTimeoutException::class.java) { client.applianceImports(url, "local-test-token", SELECTED) }
            assertNotNull(client.applianceImports(url, "local-test-token", SELECTED).context)
            repeat(2) { val request = server.takeRequest(2, TimeUnit.SECONDS)!!;assertEquals("Bearer local-test-token", request.getHeader("Authorization"));assertEquals("/api/v1/appliance/imports/context", request.path) }
        }
    }

    private class Sink : ApplianceImportAlertSink {
        val allowed = AtomicBoolean(true);val active = AtomicBoolean(false);val failNext = AtomicBoolean(false)
        val posts = CopyOnWriteArrayList<Pair<ApplianceImportAlert, Boolean>>();val cancellations = AtomicInteger()
        override fun enabled() = allowed.get()
        override fun showing() = active.get()
        override fun post(alert: ApplianceImportAlert, silent: Boolean): Boolean {
            if (failNext.compareAndSet(true, false)) error("Synthetic notification failure")
            active.set(true);posts.add(alert to silent);return true
        }
        override fun cancel() { active.set(false);cancellations.incrementAndGet() }
    }
    private fun await(condition: () -> Boolean) {
        val until = System.nanoTime() + TimeUnit.SECONDS.toNanos(3)
        while (!condition() && System.nanoTime() < until) Thread.sleep(5)
        assertTrue("Background observation deadline expired", condition())
    }
    private companion object {
        fun fixture() = JSONObject(ApplianceImportAlertsTest::class.java.getResourceAsStream("/appliance-imports.json")!!.bufferedReader().use { it.readText() })
        val PHONE = fixture().getJSONObject("context").getJSONObject("bond").getJSONObject("controller").identity()
        val PI = fixture().getJSONObject("context").getJSONObject("bond").getJSONObject("appliance").identity()
        val SELECTED = SelectedAppliance(PHONE, PI, "Pi", fixture().getJSONObject("context").getJSONObject("bond").getString("nativeGeneration"))
        fun saved() = parsePiSavedImport(fixture().getJSONObject("action"), PHONE)
        fun envelope() = JSONObject().put("schemaVersion", 1).put("applianceId", PI.deviceId).put("controllerId", PHONE.deviceId)
            .put("bondGeneration", SELECTED.confirmedGeneration).put("importsSupported", true).put("importsAvailable", true).put("context", fixture().getJSONObject("context"))
        fun state() = PiImportsState(SELECTED, parsePiImports(envelope(), SELECTED), emptyList(), true)
    }
}
