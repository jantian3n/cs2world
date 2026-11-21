package org.cs2world.client

import android.app.*
import android.content.Intent
import android.net.*
import android.os.*
import org.json.JSONObject
import java.util.concurrent.Executors

class TunnelService: VpnService() {
    companion object {
        @Volatile var status="Disconnected"
        private val lifecycle=Executors.newSingleThreadExecutor()
    }
    private val main=Handler(Looper.getMainLooper())
    private val net by lazy { getSystemService(ConnectivityManager::class.java) }
    private var bridge:NativeBridge?=null
    private var handle=0L
    private var startID=0L
    private var connectID=0L
    private var tunID=0L
    private var epoch=0
    private var currentConfig=""
    private var lastNetwork:Network?=null
    private var descriptor:ParcelFileDescriptor?=null
    private var callbackRegistered=false
    private val callback=object:ConnectivityManager.NetworkCallback(){
        override fun onAvailable(network:Network){main.post {
            if(network==lastNetwork)return@post
            val previous=lastNetwork;lastNetwork=network;setUnderlyingNetworks(arrayOf(network))
            if(previous!=null && bridge!=null) restart(currentConfig)
        }}
        override fun onLost(network:Network){main.post {
            if(lastNetwork==network){setUnderlyingNetworks(null);status="Waiting for network"}
        }}
    }
    override fun onStartCommand(intent:Intent?,flags:Int,startId:Int):Int {
        val config=intent?.getStringExtra("config") ?: runCatching { SavedConfig.read(this) }.getOrNull()
        val channel=NotificationChannel("tunnel","VPN connection",NotificationManager.IMPORTANCE_LOW)
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        val open=PendingIntent.getActivity(this,0,Intent(this,MainActivity::class.java),PendingIntent.FLAG_IMMUTABLE)
        startForeground(1,Notification.Builder(this,"tunnel").setSmallIcon(android.R.drawable.ic_lock_lock)
            .setContentTitle("CS2World VPN").setContentIntent(open).setOngoing(true).build())
        if(config.isNullOrBlank()){stopSelf();return START_NOT_STICKY}
        if(!callbackRegistered){
            net.registerNetworkCallback(NetworkRequest.Builder().addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN).build(),callback)
            callbackRegistered=true
        }
        restart(config);return START_NOT_STICKY
    }
    private fun restart(config:String){
        shutdownNative();currentConfig=config;val generation=epoch;status="Starting"
        lifecycle.execute {
            main.post {
                if(generation!=epoch)return@post
                val b=NativeBridge(this);val h=b.create()
                if(h==0L){fail("Native core unavailable");return@post}
                bridge=b;handle=h;startID=b.start(h,config)
                if(startID<0)fail("Could not queue start")
            }
        }
    }
    fun onNativeEvent(source:NativeBridge,event:String){main.post {
        if(source!==bridge)return@post
        val data=JSONObject(event);val command=data.optLong("command")
        if(command==0L)return@post
        if(!data.optBoolean("ok")){fail(data.optString("detail","Connection failed"));return@post}
        when(command){
            startID->{status="Authenticating";connectID=source.preconnect(handle);if(connectID<0)fail("Could not queue authentication")}
            connectID->try {
                val vpn=Builder().setSession("CS2World").setMtu(1500)
                    .addAddress("10.83.0.1",30).addAddress("fd42:8320::1",126)
                    .addRoute("0.0.0.0",0).addRoute("::",0)
                net.getLinkProperties(lastNetwork ?: net.activeNetwork)?.dnsServers?.forEach {vpn.addDnsServer(it)}
                vpn.addDisallowedApplication(packageName)
                descriptor=vpn.establish() ?: throw IllegalStateException("VPN permission revoked")
                tunID=source.attachTun(handle,descriptor!!.fd);if(tunID<0)fail("Could not attach TUN descriptor")
            } catch(e:Exception){fail(e.message ?: "VPN setup failed")}
            tunID->status="Connected"
        }
    }}
    private fun fail(message:String){status=message;stopSelf()}
    private fun shutdownNative(){
        epoch++;val b=bridge;val h=handle;bridge=null;handle=0;descriptor?.close();descriptor=null
        if(b!=null && h!=0L)lifecycle.execute {b.destroy(h)}
    }
    override fun onRevoke(){main.post{status="VPN permission revoked";stopSelf()}}
    override fun onDestroy(){
        if(callbackRegistered)net.unregisterNetworkCallback(callback)
        if(status=="Connected" || status=="Starting" || status=="Authenticating")status="Disconnected"
        shutdownNative();currentConfig="";stopForeground(STOP_FOREGROUND_REMOVE);super.onDestroy()
    }
}
