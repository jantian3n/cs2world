package org.cs2world.client

import android.app.Activity
import android.net.VpnService
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.*

class MainActivity: Activity() {
    private lateinit var config: EditText
    private lateinit var status: TextView
    private val main = Handler(Looper.getMainLooper())
    private val refresh = object: Runnable {
        override fun run() { status.text = TunnelService.status; main.postDelayed(this, 500) }
    }
    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        val box = LinearLayout(this).apply { orientation=LinearLayout.VERTICAL;setPadding(24,24,24,24) }
        config = EditText(this).apply {
            hint="Paste a cs2world:// link or client JSON";minLines=5
            isSaveEnabled=false
            importantForAutofill=android.view.View.IMPORTANT_FOR_AUTOFILL_NO
        }
        runCatching { SavedConfig.read(this) }.onSuccess { config.setText(it) }
            .onFailure { TunnelService.status = "Saved configuration could not be opened" }
        status=TextView(this)
        box.addView(config)
        fun button(label:String, action:()->Unit) { box.addView(Button(this).apply { text=label;setOnClickListener { action() } }) }
        button("Open JSON file") { startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).setType("*/*").addCategory(Intent.CATEGORY_OPENABLE), 2) }
        button("Connect") {
            if (config.text.isBlank() || config.length()>256*1024) { config.error="Enter a link or client JSON up to 256 KiB";return@button }
            val consent=VpnService.prepare(this)
            if (consent==null) connect() else startActivityForResult(consent,1)
        }
        button("Disconnect") { stopService(Intent(this,TunnelService::class.java)) }
        box.addView(status);setContentView(ScrollView(this).apply { addView(box) })
    }
    private fun connect() {
        runCatching { SavedConfig.write(this,config.text.toString()) }
        startForegroundService(Intent(this,TunnelService::class.java).putExtra("config",config.text.toString()))
    }
    @Deprecated("System activity result API")
    override fun onActivityResult(request:Int,result:Int,data:Intent?) {
        super.onActivityResult(request,result,data)
        if(request==1) { if(result==RESULT_OK) connect() else TunnelService.status="VPN permission declined" }
        if(request==2 && result==RESULT_OK) {
            try {
                val uri=data?.data ?: return
                contentResolver.openInputStream(uri)?.bufferedReader()?.use { reader ->
                    val buffer=CharArray(256*1024+1);var count=0
                    while(count<buffer.size){val n=reader.read(buffer,count,buffer.size-count);if(n<0) break;count+=n}
                    require(count<buffer.size) { "Configuration exceeds 256 KiB" };config.setText(String(buffer,0,count))
                }
            } catch(e:Exception) {status.text=e.message}
        }
    }
    override fun onResume(){super.onResume();main.post(refresh)}
    override fun onPause(){
        main.removeCallbacks(refresh)
        if(config.length()<=256*1024)runCatching { SavedConfig.write(this,config.text.toString()) }
        super.onPause()
    }
}
