use cs2world_ffi::*;
use serde_json::Value;
use std::{ffi::c_void, net::TcpStream, sync::mpsc, time::Duration};

extern "C" fn callback(p: *mut c_void, text: *const u8, len: usize) {
    let events = unsafe { &*p.cast::<mpsc::Sender<Value>>() };
    let data = unsafe { std::slice::from_raw_parts(text, len) };
    if let Ok(value) = serde_json::from_slice(data) {
        let _ = events.send(value);
    }
}
#[test]
fn c_abi_start_reload_rollback_stop_and_drain() {
    let h = cs2w_create();
    assert_ne!(h, 0);
    let (tx, rx) = mpsc::channel::<Value>();
    assert_eq!(
        cs2w_set_event_callback(
            h,
            Some(callback),
            (&tx as *const mpsc::Sender<Value>).cast_mut().cast()
        ),
        0
    );
    let wait = |ticket: i64| -> Value {
        assert!(ticket > 0);
        loop {
            let e = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if e["command"] == ticket {
                return e;
            }
        }
    };
    let good =
        br#"{"inbounds":[{"type":"mixed","listen_port":0}],"outbounds":[{"type":"direct"}]}"#;
    let started = wait(unsafe { cs2w_start(h, good.as_ptr(), good.len()) });
    assert_eq!(started["ok"], true);
    let addr = started["detail"]["listen"].as_str().unwrap();
    let bad = b"{}";
    assert_eq!(
        wait(unsafe { cs2w_reload(h, bad.as_ptr(), bad.len()) })["ok"],
        false
    );
    TcpStream::connect(addr).unwrap();
    let mut stats = CStats {
        version: 1,
        size: 4,
        ..Default::default()
    };
    assert_eq!(unsafe { cs2w_get_stats(h, &mut stats) }, -2);
    stats.size = std::mem::size_of::<CStats>() as u32;
    assert_eq!(unsafe { cs2w_get_stats(h, &mut stats) }, 0);
    assert_eq!(wait(cs2w_stop(h))["ok"], true);
    assert_eq!(wait(cs2w_stop(h))["ok"], true);
    assert_eq!(cs2w_set_event_callback(h, None, std::ptr::null_mut()), 0);
    drop(tx);
    assert_eq!(cs2w_destroy(h), 0);
    assert_eq!(cs2w_destroy(h), -1);
    assert_eq!(cs2w_stop(h), -1);
}
