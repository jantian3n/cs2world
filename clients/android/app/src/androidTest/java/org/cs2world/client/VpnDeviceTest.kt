package org.cs2world.client

import android.content.Intent
import android.net.VpnService
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class VpnDeviceTest {
    @Test fun realTunCarriesTrafficFromShellUid() {
        val args=InstrumentationRegistry.getArguments()
        val text=args.getString("config64")
        assumeTrue("Run through clients/android/test_device.py",text!=null)
        val ins=InstrumentationRegistry.getInstrumentation()
        val context=ins.targetContext
        val config=String(Base64.decode(text,Base64.DEFAULT),Charsets.UTF_8)
        val intent=Intent(context,MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val activity=ins.startActivitySync(intent)
        val vpn=Intent(context,TunnelService::class.java)
        try {
            assertNull("VPN permission is required on the test AVD",VpnService.prepare(context))
            context.startForegroundService(vpn.putExtra("config",config))
            val deadline=SystemClock.elapsedRealtime()+20000
            while(TunnelService.status!="Connected" && SystemClock.elapsedRealtime()<deadline)Thread.sleep(100)
            assertEquals("Connected",TunnelService.status)
            val tcp=args.getString("tcp")!!.toInt();val udp=args.getString("udp")!!.toInt()
            val token=args.getString("token")!!;require(Regex("[a-z0-9]+").matches(token))
            val apk=ins.context.applicationInfo.sourceDir
            val cmd="env CLASSPATH=$apk app_process /system/bin org.cs2world.client.ExternalProbe 198.18.0.42 $tcp $udp $token"
            val result=ParcelFileDescriptor.AutoCloseInputStream(ins.uiAutomation.executeShellCommand(cmd)).bufferedReader().use {it.readText()}
            assertTrue(result,result.contains("TCP PASS uid=2000"))
            assertTrue(result,result.contains("UDP PASS uid=2000"))
        } finally {
            context.stopService(vpn)
            ins.runOnMainSync{activity.finish()}
        }
    }
}
