package org.cs2world.client

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal object SavedConfig {
    private const val alias = "org.cs2world.client.config"
    @Synchronized private fun key(): SecretKey {
        val store=KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias,null) as? SecretKey)?.let { return it }
        val spec=KeyGenParameterSpec.Builder(alias,KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES,"AndroidKeyStore").run { init(spec); generateKey() }
    }
    fun read(context:Context):String {
        val prefs=context.getSharedPreferences("connection",Context.MODE_PRIVATE)
        val iv=prefs.getString("iv",null) ?: return ""
        val body=prefs.getString("body",null) ?: return ""
        return Cipher.getInstance("AES/GCM/NoPadding").run {
            init(Cipher.DECRYPT_MODE,key(),GCMParameterSpec(128,Base64.decode(iv,Base64.NO_WRAP)))
            doFinal(Base64.decode(body,Base64.NO_WRAP)).toString(Charsets.UTF_8)
        }
    }
    fun write(context:Context,text:String) {
        val prefs=context.getSharedPreferences("connection",Context.MODE_PRIVATE)
        if(text.isEmpty()){prefs.edit().clear().apply();return}
        val cipher=Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE,key()) }
        val encrypted=cipher.doFinal(text.toByteArray(Charsets.UTF_8))
        prefs.edit().putString("iv",Base64.encodeToString(cipher.iv,Base64.NO_WRAP))
            .putString("body",Base64.encodeToString(encrypted,Base64.NO_WRAP)).apply()
    }
}
