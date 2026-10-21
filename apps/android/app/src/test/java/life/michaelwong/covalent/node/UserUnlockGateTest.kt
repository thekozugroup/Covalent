package life.michaelwong.covalent.node

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class UserUnlockGateTest {
    @Test
    fun anAlreadyUnlockedUserNeedsNoObserver() {
        var calls = 0
        runAfterUserUnlock({ true }, { error("No observer is needed") }, { calls++ })
        assertEquals(1, calls)
    }

    @Test
    fun unlockBetweenInitialCheckAndRegistrationIsNotLost() {
        var unlocked = false
        val events = mutableListOf<String>()
        runAfterUserUnlock(
            { unlocked },
            {
                // Android has sent its non-sticky broadcast before registration completes.
                unlocked = true
                events += "registered"
                { events += "unregistered" }
            },
            { events += "action" },
        )
        assertEquals(listOf("registered", "unregistered", "action"), events)
    }

    @Test
    fun aCallbackDuringRegistrationWaitsForItsUnregisterHandle() {
        var unlocked = false
        val events = mutableListOf<String>()
        runAfterUserUnlock(
            { unlocked },
            { notify ->
                unlocked = true
                notify()
                assertTrue(events.isEmpty())
                events += "registered"
                { events += "unregistered" }
            },
            { events += "action" },
        )
        assertEquals(listOf("registered", "unregistered", "action"), events)
    }

    @Test
    fun lockedOrRepeatedNotificationsCannotStartTheAction() {
        var unlocked = false
        var notify: () -> Unit = { error("Not registered") }
        var calls = 0
        var removals = 0
        runAfterUserUnlock({ unlocked }, { callback ->
            notify = callback
            { removals++ }
        }, { calls++ })
        notify()
        assertEquals(0, calls)
        assertEquals(0, removals)
        unlocked = true
        repeat(5) { notify() }
        assertEquals(1, calls)
        assertEquals(1, removals)
    }

    @Test
    fun racingNotificationsUnregisterAndStartOnlyOnce() {
        val unlocked = java.util.concurrent.atomic.AtomicBoolean(false)
        val calls = AtomicInteger()
        val removals = AtomicInteger()
        var notify: () -> Unit = { error("Not registered") }
        runAfterUserUnlock(unlocked::get, { callback ->
            notify = callback
            { removals.incrementAndGet(); Unit }
        }, { calls.incrementAndGet() })
        val pool = Executors.newFixedThreadPool(8)
        try {
            val start = CountDownLatch(1)
            val results = (1..8).map {
                pool.submit {
                    check(start.await(2, TimeUnit.SECONDS))
                    repeat(100) { notify() }
                }
            }
            unlocked.set(true)
            start.countDown()
            results.forEach { it.get(3, TimeUnit.SECONDS) }
        } finally {
            pool.shutdownNow()
            assertTrue(pool.awaitTermination(3, TimeUnit.SECONDS))
        }
        assertEquals(1, calls.get())
        assertEquals(1, removals.get())
    }

    @Test
    fun anActionFailureDoesNotLeaveAnObserverOrReplayTheAction() {
        var unlocked = false
        var notify: () -> Unit = { error("Not registered") }
        var removed = false
        var calls = 0
        val failure = IllegalStateException("synthetic action failure")
        runAfterUserUnlock({ unlocked }, { callback ->
            notify = callback
            { removed = true }
        }, { assertTrue(removed); calls++; throw failure })
        unlocked = true
        assertEquals(failure, runCatching { notify() }.exceptionOrNull())
        notify()
        assertEquals(1, calls)
    }

    @Test
    fun registrationFailureDoesNotRunTheAction() {
        var called = false
        val failure = IllegalStateException("synthetic registration failure")
        assertEquals(failure, runCatching {
            runAfterUserUnlock({ false }, { throw failure }, { called = true })
        }.exceptionOrNull())
        assertFalse(called)
    }
}
