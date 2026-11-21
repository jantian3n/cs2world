package org.cs2world.client

class NativeBridge(private val protectFd:(Int)->Boolean, private val dispatch:(NativeBridge,String)->Unit) {
    constructor(service: TunnelService):this(service::protect,service::onNativeEvent)
    companion object { init { System.loadLibrary("cs2world_ffi") } }
    fun protectSocket(fd: Int): Boolean = protectFd(fd)
    fun onEvent(json: String) = dispatch(this,json)
    external fun create(): Long
    external fun start(handle: Long, config: String): Long
    external fun preconnect(handle: Long): Long
    external fun attachTun(handle: Long, fd: Int): Long
    external fun stats(handle: Long): LongArray?
    external fun stop(handle: Long): Long
    external fun destroy(handle: Long): Int
}
