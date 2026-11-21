package org.cs2world.client

import androidx.test.ext.junit.runners.AndroidJUnit4
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NativeBridgeTest {
    @Test fun nativeLifecycleCallbacksAndProtectedSocksConnect() {
        val events=LinkedBlockingQueue<JSONObject>()
        val protected=AtomicInteger()
        val bridge=NativeBridge({fd -> protected.incrementAndGet();fd>=0},{_,event -> events.put(JSONObject(event))})
        val h=bridge.create()
        assertTrue(h>0)
        fun waitFor(id:Long):JSONObject {
            assertTrue(id>0)
            while(true){
                val e=events.poll(8,TimeUnit.SECONDS) ?: throw AssertionError("native command timed out")
                if(e.getLong("command")==id){assertTrue(e.toString(),e.getBoolean("ok"));return e}
            }
        }
        try {
            val config="""{"inbounds":[{"type":"mixed","listen_port":0}],"outbounds":[{"type":"direct"}]}"""
            val listen=waitFor(bridge.start(h,config)).getJSONObject("detail").getString("listen")
            val local=java.util.Collections.list(java.net.NetworkInterface.getNetworkInterfaces())
                .filter { it.isUp && !it.isLoopback }.flatMap { java.util.Collections.list(it.inetAddresses) }
                .first {it is java.net.Inet4Address && !it.isLoopbackAddress}
            ServerSocket(0,1,local).use { origin ->
                origin.soTimeout=8000
                Socket("127.0.0.1",listen.substringAfterLast(':').toInt()).use { client ->
                    client.soTimeout=8000
                    val input=java.io.DataInputStream(client.getInputStream())
                    val out=client.getOutputStream()
                    out.write(byteArrayOf(5,1,0))
                    val methods=ByteArray(2);input.readFully(methods)
                    assertArrayEquals(byteArrayOf(5,0),methods)
                    out.write(byteArrayOf(5,1,0,1)+local.address+byteArrayOf((origin.localPort shr 8).toByte(),origin.localPort.toByte()))
                    val reply=ByteArray(10);input.readFully(reply);assertEquals(0,reply[1].toInt())
                    origin.accept().use { peer ->
                        peer.soTimeout=8000;out.write(0x5a);assertEquals(0x5a,peer.getInputStream().read())
                        peer.getOutputStream().write(0x3c);assertEquals(0x3c,input.read())
                    }
                }
            }
            assertTrue(protected.get()>0)
            assertEquals(8,bridge.stats(h)!!.size)
            waitFor(bridge.stop(h));waitFor(bridge.stop(h))
        } finally {assertEquals(0,bridge.destroy(h))}
    }
}
