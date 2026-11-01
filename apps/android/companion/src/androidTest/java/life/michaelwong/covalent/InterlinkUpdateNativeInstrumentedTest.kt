package life.michaelwong.covalent

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Process
import android.system.Os
import android.system.OsConstants
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.node.NodeProviderService
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

/** Empty owned QA package only. Exercises real JNI/private Android IO, not a Pi change. */
class InterlinkUpdateNativeInstrumentedTest {
    private val instrumentation=InstrumentationRegistry.getInstrumentation()
    private val context=instrumentation.targetContext
    private fun await(condition:()->Boolean) {
        val deadline=System.nanoTime()+30_000_000_000L
        while(System.nanoTime()<deadline) {if(condition())return;Thread.sleep(100)}
        error("Native update journal fixture did not settle")
    }
    @Test fun nativeUpdateJournalSurvivesActualHostStopAndRestartWithoutChangingIdentity() {
        check(InstrumentationRegistry.getArguments().getString("interlinkOwnedFixture")=="true" &&
            Build.HARDWARE=="ranchu" && Build.FINGERPRINT.startsWith("google/sdk_gphone")) {
            "Only the admitted empty emulator fixture may start or stop this host"
        }
        if(Build.VERSION.SDK_INT>=37) assertEquals("Run the required permission-UI startup fixture first",
            PackageManager.PERMISSION_GRANTED,context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK))
        val app=context.applicationContext as InterlinkApplication
        val client=CovalentNodeClient(500)
        context.startActivity(context.packageManager.getLaunchIntentForPackage(context.packageName)!!.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        await {app.manager.value!=null}
        val manager=checkNotNull(app.manager.value)
        try {
            assertTrue(manager.enableFolderSyncHost())
            await {manager.localConnectionForFolderSync()?.let {runCatching {client.applianceContext(it.baseUrl,it.token)}.isSuccess}==true}
            val first=checkNotNull(manager.localConnectionForFolderSync())
            val identity=client.applianceContext(first.baseUrl,first.token).local
            assertTrue(client.applianceUpdateActions(first.baseUrl,first.token,identity).isEmpty())
            val file=File(context.noBackupFilesDir,"covalent-node/appliance-update-outbox.json")
            val stat=Os.lstat(file.path);assertTrue(OsConstants.S_ISREG(stat.st_mode));assertEquals(Process.myUid(),stat.st_uid)
            assertEquals(0,stat.st_mode and 0x3f);assertEquals(1L,stat.st_nlink)
            val bytes=file.readBytes();val saved=JSONObject(String(bytes,Charsets.UTF_8))
            assertEquals(1,saved.getInt("schemaVersion"));assertEquals(identity.deviceId,saved.getJSONObject("controller").getString("deviceId"))
            assertEquals(0,saved.getJSONArray("actions").length())
            context.stopService(Intent(context,NodeProviderService::class.java))
            await {runCatching {client.applianceContext(first.baseUrl,first.token)}.isFailure}
            assertTrue(manager.enableFolderSyncHost())
            await {manager.localConnectionForFolderSync()?.let {runCatching {client.applianceContext(it.baseUrl,it.token)}.isSuccess}==true}
            val next=checkNotNull(manager.localConnectionForFolderSync())
            assertEquals(identity,client.applianceContext(next.baseUrl,next.token).local)
            assertTrue(client.applianceUpdateActions(next.baseUrl,next.token,identity).isEmpty())
            assertArrayEquals(bytes,file.readBytes());assertEquals(stat.st_ino,Os.lstat(file.path).st_ino)
        } finally {
            context.stopService(Intent(context,NodeProviderService::class.java))
            manager.localConnectionForFolderSync()?.let {connection->await {runCatching {client.applianceContext(connection.baseUrl,connection.token)}.isFailure}}
        }
    }
}
