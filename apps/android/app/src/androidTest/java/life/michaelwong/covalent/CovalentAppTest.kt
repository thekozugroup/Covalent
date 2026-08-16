package life.michaelwong.covalent

import android.app.job.JobInfo
import android.content.ComponentName
import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.os.Build
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.ui.CovalentApp
import life.michaelwong.covalent.ui.PrimaryActionToolbar
import life.michaelwong.covalent.ui.validateAndPersistSetup
import life.michaelwong.covalent.ui.theme.CovalentTheme
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.work.TransferJobService
import life.michaelwong.covalent.work.TransferScheduler
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class CovalentAppTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun firstLaunchRequiresDirectNodeConnection() {
        val isolatedStore = isolatedStore("first_launch")
        compose.setContent { CovalentTheme { CovalentApp(isolatedStore) } }
        compose.onNodeWithText("Connect your local node").assertIsDisplayed()
        compose.onNodeWithText("Local node token").assertIsDisplayed()
    }

    @Test
    fun failedSetupValidationDoesNotPersistCredentials() {
        val store = isolatedStore("failed_setup")
        val result = runCatching {
            validateAndPersistSetup(
                CovalentNodeClient(),
                store,
                "Untrusted input",
                "not-a-url",
                "must-not-be-saved",
            )
        }

        assertTrue(result.isFailure)
        assertEquals("", store.baseUrl)
        assertEquals("", store.displayName)
        assertEquals("", store.token)
    }

    @Test
    fun floatingToolbarKeepsTheThreeTierOneActionsAccessible() {
        compose.setContent { CovalentTheme { PrimaryActionToolbar(enabled = true, onAction = {}) } }
        compose.onNodeWithText("Pair").assertIsDisplayed()
        compose.onNodeWithText("Backup").assertIsDisplayed()
        compose.onNodeWithText("Restore").assertIsDisplayed()
    }

    @Test
    fun api34PlusTransferJobIsUserInitiatedPersistedAndNetworkBound() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.UPSIDE_DOWN_CAKE) return
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val job = TransferScheduler.buildUserInitiatedJob(context, "backup-regression", 42_001)

        assertTrue(job.isUserInitiated)
        assertTrue(job.isPersisted)
        assertNotNull(job.requiredNetwork)
        assertEquals(ComponentName(context, TransferJobService::class.java), job.service)
        assertEquals("backup-regression", job.extras.getString("job_id"))
        assertEquals(JobInfo.PRIORITY_MAX, job.priority)
    }


    private fun isolatedStore(suffix: String): SecureNodeStore {
        val base = InstrumentationRegistry.getInstrumentation().targetContext
        val isolatedContext = object : ContextWrapper(base) {
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                base.getSharedPreferences("instrumentation_${suffix}_$name", Context.MODE_PRIVATE)
        }
        isolatedContext.getSharedPreferences("covalent_node", Context.MODE_PRIVATE)
            .edit().clear().commit()
        return SecureNodeStore(isolatedContext)
    }
}
