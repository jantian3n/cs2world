use super::*;
use jni::{
    objects::{GlobalRef, JObject, JString, JValue},
    sys::{jint, jlong, jlongArray},
    JNIEnv, JavaVM,
};
use std::os::fd::{FromRawFd, OwnedFd};

struct Bridge {
    vm: JavaVM,
    object: GlobalRef,
}
static BRIDGE: Mutex<Option<(u64, Arc<Bridge>)>> = Mutex::new(None);
extern "C" fn event(ctx: *mut c_void, bytes: *const u8, len: usize) {
    guarded((), || {
        let bridge = unsafe { &*ctx.cast::<Bridge>() };
        let Ok(mut env) = bridge.vm.attach_current_thread() else {
            return;
        };
        let text = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(bytes, len) });
        if let Ok(s) = env.new_string(&*text) {
            let _ = env.call_method(
                &bridge.object,
                "onEvent",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&s)],
            );
            let _ = env.delete_local_ref(s);
        }
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
    });
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_create(
    env: JNIEnv<'_>,
    obj: JObject<'_>,
) -> jlong {
    guarded(0, || {
        let mut active = BRIDGE.lock().unwrap();
        if active.is_some() {
            return 0;
        }
        let (Ok(vm), Ok(object)) = (env.get_java_vm(), env.new_global_ref(obj)) else {
            return 0;
        };
        let bridge = Arc::new(Bridge { vm, object });
        let id = cs2w_create();
        if id == 0 {
            return 0;
        }
        if cs2w_set_event_callback(id, Some(event), Arc::as_ptr(&bridge) as *mut c_void) != 0 {
            cs2w_destroy(id);
            return 0;
        }
        let vpn = bridge.clone();
        cs2world_core::platform_socket::set_protector(Some(Arc::new(move |fd| {
            let Ok(mut env) = vpn.vm.attach_current_thread() else {
                return false;
            };
            let result = env
                .call_method(&vpn.object, "protectSocket", "(I)Z", &[JValue::Int(fd)])
                .and_then(|v| v.z())
                .unwrap_or(false);
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            result
        })));
        *active = Some((id, bridge));
        id as i64
    })
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_start(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    id: jlong,
    config: JString<'_>,
) -> jlong {
    guarded(-4, || match env.get_string(&config) {
        Ok(s) => queue(id as u64, Cmd::Start(s.into())),
        Err(_) => -2,
    })
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_attachTun(
    _: JNIEnv<'_>,
    _: JObject<'_>,
    id: jlong,
    fd: jint,
) -> jlong {
    guarded(-4, || {
        if fd < 0 {
            return -2;
        }
        let owned = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if owned < 0 {
            return -2;
        }
        queue(
            id as u64,
            Cmd::AndroidTun(unsafe { OwnedFd::from_raw_fd(owned) }),
        )
    })
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_stop(
    _: JNIEnv<'_>,
    _: JObject<'_>,
    id: jlong,
) -> jlong {
    cs2w_stop(id as u64)
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_preconnect(
    _: JNIEnv<'_>,
    _: JObject<'_>,
    id: jlong,
) -> jlong {
    cs2w_preconnect(id as u64)
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_stats(
    env: JNIEnv<'_>,
    _: JObject<'_>,
    id: jlong,
) -> jlongArray {
    guarded(ptr::null_mut(), || {
        let Ok(h) = get(id as u64) else {
            return ptr::null_mut();
        };
        let s = *h.stats.lock().unwrap();
        let vals = [
            s.up_total,
            s.down_total,
            s.up_bps,
            s.down_bps,
            s.active_tcp,
            s.active_udp,
            s.wire_up,
            s.wire_down,
        ]
        .map(|v| v as i64);
        let Ok(array) = env.new_long_array(vals.len() as i32) else {
            return ptr::null_mut();
        };
        if env.set_long_array_region(&array, 0, &vals).is_err() {
            return ptr::null_mut();
        }
        array.into_raw()
    })
}
#[no_mangle]
pub extern "system" fn Java_org_cs2world_client_NativeBridge_destroy(
    _: JNIEnv<'_>,
    _: JObject<'_>,
    id: jlong,
) -> jint {
    guarded(-4, || {
        let mut active = BRIDGE.lock().unwrap();
        if !active.as_ref().is_some_and(|(h, _)| *h == id as u64) {
            return -1;
        }
        let code = cs2w_destroy(id as u64);
        if code == 0 {
            cs2world_core::platform_socket::set_protector(None);
            active.take();
        }
        code
    })
}
