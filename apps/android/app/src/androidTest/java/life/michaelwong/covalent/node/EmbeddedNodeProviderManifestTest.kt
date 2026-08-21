package life.michaelwong.covalent.node

import android.Manifest
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.ParcelFileDescriptor
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.data.SecureNodeStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** API 37 contract checks for the explicit, user-stoppable provider service. */
class EmbeddedNodeProviderManifestTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun api37DeclaresConnectedDeviceForegroundServiceAndLocalNetworkPermission() {
        val packageManager = context.packageManager
        assertEquals(
            PackageManager.PERMISSION_GRANTED,
            packageManager.checkPermission(
                Manifest.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE,
                context.packageName,
            ),
        )
        // ACCESS_LOCAL_NETWORK is a dangerous permission from API 37, so it is never granted
        // at install time. The manifest contract is that it is declared and therefore
        // grantable - `pm grant` fails outright for a permission the package never requested.
        val requested = packageManager
            .getPackageInfo(context.packageName, PackageManager.GET_PERMISSIONS)
            .requestedPermissions
            .orEmpty()
        assertTrue(Manifest.permission.ACCESS_LOCAL_NETWORK in requested)

        val instrumentation = InstrumentationRegistry.getInstrumentation()
        ParcelFileDescriptor.AutoCloseInputStream(
            instrumentation.uiAutomation.executeShellCommand(
                "pm grant ${context.packageName} ${Manifest.permission.ACCESS_LOCAL_NETWORK}",
            ),
        ).use { it.readBytes() }
        assertEquals(
            PackageManager.PERMISSION_GRANTED,
            packageManager.checkPermission(
                Manifest.permission.ACCESS_LOCAL_NETWORK,
                context.packageName,
            ),
        )
        val service = packageManager.getServiceInfo(
            ComponentName(context, NodeProviderService::class.java),
            PackageManager.GET_META_DATA,
        )
        val connectedDeviceType =
            service.foregroundServiceType and ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE, connectedDeviceType)
        assertTrue(!service.exported)
    }

    @Test
    fun providerStartAndStopUseExplicitServiceActions() {
        val start = Intent(context, NodeProviderService::class.java).setAction(NodeProviderService.ACTION_START)
        val stop = Intent(context, NodeProviderService::class.java).setAction(NodeProviderService.ACTION_STOP)
        assertEquals(NodeProviderService.ACTION_START, start.action)
        assertEquals(NodeProviderService.ACTION_STOP, stop.action)
        assertEquals(ComponentName(context, NodeProviderService::class.java), start.component)
        assertEquals(ComponentName(context, NodeProviderService::class.java), stop.component)
    }

    @Test
    fun disablingPhoneProviderPreservesExternalControllerCredentials() {
        val external = SecureNodeStore(context)
        val originalBaseUrl = external.baseUrl
        val originalToken = external.token
        val originalName = external.displayName
        try {
            external.baseUrl = "https://external.example.test"
            external.token = "external-controller-token-for-test-only"
            external.displayName = "External controller"

            EmbeddedNodeManager(context).apply {
                selectExternalMode()
                disable()
            }

            assertEquals("https://external.example.test", external.baseUrl)
            assertEquals("external-controller-token-for-test-only", external.token)
            assertEquals("External controller", external.displayName)
        } finally {
            external.baseUrl = originalBaseUrl
            external.token = originalToken
            external.displayName = originalName
        }
    }

    @Test
    fun selectedLocalWithoutLocalCredentialsNeverFallsBackToExternalController() {
        val external = SecureNodeStore(context)
        val originalBaseUrl = external.baseUrl
        val originalToken = external.token
        val preferences = context.getSharedPreferences("covalent_embedded_provider", Context.MODE_PRIVATE)
        val localPreferences = context.getSharedPreferences("covalent_embedded_node_credentials", Context.MODE_PRIVATE)
        val originalMode = preferences.getString("active_mode", "external")
        val originalLocalBaseUrl = localPreferences.getString("base_url", null)
        val originalLocalToken = localPreferences.getString("token", null)
        try {
            external.baseUrl = "https://external.example.test"
            external.token = "external-controller-token-for-test-only"
            preferences.edit().putString("active_mode", "local").apply()
            localPreferences.edit().clear().apply()

            assertTrue(ActiveNodeConnectionResolver(context).activeConnection(external) == null)
        } finally {
            external.baseUrl = originalBaseUrl
            external.token = originalToken
            preferences.edit().putString("active_mode", originalMode).apply()
            localPreferences.edit().clear().apply()
            localPreferences.edit().apply {
                originalLocalBaseUrl?.let { putString("base_url", it) }
                originalLocalToken?.let { putString("token", it) }
            }.apply()
        }
    }

    @Test
    fun selectingExternalRetainsRunningProviderStateAndDoesNotRewriteExternalCredentials() {
        val external = SecureNodeStore(context)
        val preferences = context.getSharedPreferences("covalent_embedded_provider", Context.MODE_PRIVATE)
        val originalEnabled = preferences.getBoolean("enabled", false)
        val originalRunning = preferences.getBoolean("running", false)
        val originalMode = preferences.getString("active_mode", "external") ?: "external"
        val originalBaseUrl = external.baseUrl
        val originalToken = external.token
        try {
            external.baseUrl = "https://external.example.test"
            external.token = "external-controller-token-for-test-only"
            preferences.edit().putBoolean("enabled", true).putBoolean("running", true).apply()

            val manager = EmbeddedNodeManager(context)
            manager.selectExternalMode()

            assertTrue(manager.state.value.running)
            assertEquals("https://external.example.test", external.baseUrl)
            assertEquals("external-controller-token-for-test-only", external.token)
        } finally {
            preferences.edit()
                .putBoolean("enabled", originalEnabled)
                .putBoolean("running", originalRunning)
                .putString("active_mode", originalMode)
                .apply()
            external.baseUrl = originalBaseUrl
            external.token = originalToken
        }
    }

    @Test
    fun localSelectionRequiresReadyLocalCredentialsAndKeyProtectionGatePreventsEnable() {
        val preferences = context.getSharedPreferences("covalent_embedded_provider", Context.MODE_PRIVATE)
        val originalEnabled = preferences.getBoolean("enabled", false)
        val originalRunning = preferences.getBoolean("running", false)
        preferences.edit().putBoolean("enabled", true).putBoolean("running", true).apply()
        val manager = EmbeddedNodeManager(context)
        try {
            assertTrue(!manager.selectLocalMode())
            assertTrue(!manager.keyProtectionAvailable())

            manager.enable(
                maxBytes = 2L * 1024L * 1024L * 1024L,
                keepFreeBytes = 512L * 1024L * 1024L,
            )
            assertTrue(!manager.state.value.enabled)
            assertTrue(!manager.state.value.running)
        } finally {
            preferences.edit().putBoolean("enabled", originalEnabled).putBoolean("running", originalRunning).apply()
        }
    }

    @Test
    fun providerCapacityRejectsInvalidHeadroomAndUnallocatableLimits() {
        val manager = EmbeddedNodeManager(context)
        assertTrue(manager.capacityValidationMessage(511L * 1024L * 1024L, 0L) != null)
        assertTrue(manager.capacityValidationMessage(Long.MAX_VALUE, 0L) != null)
    }
}
