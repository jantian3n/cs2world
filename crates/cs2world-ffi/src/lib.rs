//! C ABI. Callers retain ownership of input/output buffers. Each handle serializes
//! commands on a worker and dispatches callbacks on a separate native thread.
use cs2world_core::{
    config::Config,
    service::{Service, Stats},
};
use serde_json::{json, Value};
use std::{
    cell::Cell,
    collections::HashMap,
    ffi::c_void,
    io,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock,
    },
    thread,
};
use tokio::sync::mpsc as channel;

#[cfg(target_os = "android")]
mod android;
type Callback = extern "C" fn(*mut c_void, *const u8, usize);
type Slot = Option<(Callback, usize)>;
enum Dispatch {
    Message(bool, Value),
    Set(bool, Slot, mpsc::Sender<()>),
    Quit,
}
enum Cmd {
    Start(String),
    Reload(String),
    Stop,
    Preconnect,
    Probe(String, u16),
    DesktopTun,
    #[cfg(target_os = "android")]
    AndroidTun(std::os::fd::OwnedFd),
    Quit,
}
struct Handle {
    tx: channel::Sender<(i64, Cmd)>,
    output: SyncSender<Dispatch>,
    stats: Arc<Mutex<Stats>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    dispatch: Mutex<Option<thread::JoinHandle<()>>>,
}
static ALL: OnceLock<Mutex<HashMap<u64, Arc<Handle>>>> = OnceLock::new();
static IDS: AtomicU64 = AtomicU64::new(1);
thread_local! {static IN_CALLBACK:Cell<bool>=const{Cell::new(false)};}
fn next() -> u64 {
    IDS.fetch_add(1, Ordering::Relaxed)
}
fn handles() -> &'static Mutex<HashMap<u64, Arc<Handle>>> {
    ALL.get_or_init(Default::default)
}
fn get(id: u64) -> Result<Arc<Handle>, i64> {
    handles()
        .lock()
        .map_err(|_| -4i64)?
        .get(&id)
        .cloned()
        .ok_or(-1)
}
fn guarded<T>(error: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(error)
}
fn queue(id: u64, cmd: Cmd) -> i64 {
    // Removing the handle for destroy and enqueueing accepted commands are serialized.
    let Ok(registry) = handles().lock() else {
        return -4;
    };
    let Some(h) = registry.get(&id) else {
        return -1;
    };
    let ticket = next() as i64;
    match h.tx.try_send((ticket, cmd)) {
        Ok(()) => ticket,
        Err(channel::error::TrySendError::Full(_)) => -3,
        Err(_) => -4,
    }
}
unsafe fn input(p: *const u8, n: usize) -> Result<String, i64> {
    if p.is_null() || n == 0 || n > 4 * 1024 * 1024 {
        return Err(-2);
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(p, n) })
        .map(str::to_owned)
        .map_err(|_| -2)
}
fn error(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn dispatcher(rx: mpsc::Receiver<Dispatch>) {
    let mut slots: [Slot; 2] = [None, None];
    for msg in rx {
        match msg {
            Dispatch::Message(log, body) => {
                if let Some((func, user)) = slots[log as usize] {
                    let text = body.to_string();
                    IN_CALLBACK.with(|v| v.set(true));
                    func(user as *mut c_void, text.as_ptr(), text.len());
                    IN_CALLBACK.with(|v| v.set(false));
                }
            }
            Dispatch::Set(log, slot, ack) => {
                slots[log as usize] = slot;
                let _ = ack.send(());
            }
            Dispatch::Quit => break,
        }
    }
}

async fn worker(
    mut rx: channel::Receiver<(i64, Cmd)>,
    output: SyncSender<Dispatch>,
    stats: Arc<Mutex<Stats>>,
) {
    let mut service: Option<Service> = None;
    let mut logs: Option<tokio::sync::broadcast::Receiver<String>> = None;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let mut node: Option<(String, u16)> = None;
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "android"))]
    let mut tun: Option<cs2world_core::tun::TunSession> = None;
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        let (ticket, cmd) = tokio::select! {
            biased;
            msg=rx.recv()=>match msg{Some(msg)=>msg,None=>break},
            _=timer.tick()=>{if let Some(s)=&service{if let Ok(mut v)=stats.lock(){*v=s.snapshot()}}continue},
            line=async{match &mut logs{Some(r)=>r.recv().await,None=>std::future::pending().await}}=>{
                match line{
                    Ok(line)=>{let _=output.try_send(Dispatch::Message(true,json!({"message":line})));let _=output.try_send(Dispatch::Message(false,json!({"command":0,"ok":true,"detail":line})));},
                    Err(tokio::sync::broadcast::error::RecvError::Closed)=>logs=None,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{},
                }continue
            }
        };
        if matches!(cmd, Cmd::Quit) {
            break;
        }
        let outcome:io::Result<Value>=async {
            match cmd{
                Cmd::Start(text)=>{
                    if service.is_some(){return Err(error("service is running"));}
                    let cfg=Config::from_input(&text)?;
                    #[cfg(any(target_os="macos",target_os="windows"))]
                    let ep=match &cfg{Config::Client(c)=>c.node.as_ref().map(|n|(n.server.clone(),n.server_port)),_=>None};
                    let created=Service::start(cfg).await?;
                    #[cfg(any(target_os="macos",target_os="windows"))] {node=ep;}
                    let addr=created.local_addr();logs=Some(created.subscribe());service=Some(created);
                    Ok(json!({"state":"running","listen":addr.to_string()}))
                }
                Cmd::Reload(text)=>{
                    let cfg=Config::from_input(&text)?;
                    #[cfg(any(target_os="macos",target_os="windows"))]
                    let ep=match &cfg{Config::Client(c)=>c.node.as_ref().map(|n|(n.server.clone(),n.server_port)),_=>None};
                    let s=service.as_mut().ok_or_else(||error("service is stopped"))?;
                    #[cfg(any(target_os="macos",target_os="windows",target_os="android"))]
                    if tun.is_some() {
                        let Config::Client(client)=&cfg else{return Err(error("TUN requires a Mixed client"))};
                        let bound=s.local_addr();
                        if client.listen.ip()!=bound.ip() || (client.listen.port()!=0 && client.listen.port()!=bound.port()){
                            return Err(error("stop TUN before changing its Mixed listener"));
                        }
                        #[cfg(any(target_os="macos",target_os="windows"))]
                        if ep!=node{return Err(error("stop desktop TUN before changing its endpoint route"))}
                    }
                    s.reload(cfg).await?;logs=Some(s.subscribe());
                    #[cfg(any(target_os="macos",target_os="windows"))] {node=ep;}
                    Ok(json!("reloaded"))
                }
                Cmd::Stop=>{
                    #[cfg(any(target_os="macos",target_os="windows",target_os="android"))]
                    if let Some(mut t)=tun.take(){let _=t.stop().await;}
                    if let Some(mut s)=service.take(){s.stop().await;if let Ok(mut v)=stats.lock(){*v=s.snapshot();}}logs=None;Ok(json!("stopped"))
                }
                Cmd::Preconnect=>{service.as_ref().ok_or_else(||error("service is stopped"))?.preconnect().await?;Ok(json!("authenticated"))}
                Cmd::Probe(host,port)=>{let p=cs2world_core::probe::probe(&host,port).await?;Ok(json!({"rtt_ms":p.rtt_ms,"server_name":p.info.server_name,"game":p.info.game,"app_id":p.info.app_id,"map":p.info.map_name,"players":p.info.players,"max_players":p.info.max_players}))}
                Cmd::DesktopTun=>{
                    #[cfg(any(target_os="macos",target_os="windows"))] {
                        if tun.is_some(){return Err(error("TUN is running"))}
                        let s=service.as_ref().ok_or_else(||error("service is stopped"))?;
                        let (host,port)=node.as_ref().ok_or_else(||error("TUN requires a client node"))?;
                        let ep=tokio::net::lookup_host((host.as_str(),*port)).await?.next().ok_or_else(||error("node has no address"))?;
                        tun=Some(cs2world_core::tun::start_desktop(s.local_addr(),ep).await?);Ok(json!("tun_running"))
                    }
                    #[cfg(not(any(target_os="macos",target_os="windows")))] {Err(error("desktop TUN unavailable on this target"))}
                }
                #[cfg(target_os="android")] Cmd::AndroidTun(fd)=>{
                    if tun.is_some(){return Err(error("TUN is running"))}
                    let s=service.as_ref().ok_or_else(||error("service is stopped"))?;
                    tun=Some(cs2world_core::tun::start_fd(s.local_addr(),fd).await?);Ok(json!("tun_running"))
                }
                Cmd::Quit=>unreachable!(),
            }
        }.await;
        let event = match outcome {
            Ok(value) => json!({"command":ticket,"ok":true,"detail":value}),
            Err(e) => json!({"command":ticket,"ok":false,"detail":e.to_string()}),
        };
        if output.send(Dispatch::Message(false, event)).is_err() {
            break;
        }
    }
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "android"))]
    if let Some(mut t) = tun.take() {
        let _ = t.stop().await;
    }
    if let Some(mut s) = service.take() {
        s.stop().await;
    }
}

#[no_mangle]
pub extern "C" fn cs2w_abi_version() -> u32 {
    1
}
#[no_mangle]
pub extern "C" fn cs2w_create() -> u64 {
    guarded(0, || {
        let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            return 0;
        };
        let (tx, rx) = channel::channel(64);
        let (output, events) = mpsc::sync_channel(128);
        let stats = Arc::new(Mutex::new(Stats::default()));
        let Ok(dispatch) = thread::Builder::new()
            .name("cs2w-callback".into())
            .spawn(move || dispatcher(events))
        else {
            return 0;
        };
        let out = output.clone();
        let st = stats.clone();
        let Ok(worker) = thread::Builder::new()
            .name("cs2w-command".into())
            .spawn(move || rt.block_on(worker(rx, out, st)))
        else {
            return 0;
        };
        let id = next();
        let h = Arc::new(Handle {
            tx,
            output,
            stats,
            worker: Mutex::new(Some(worker)),
            dispatch: Mutex::new(Some(dispatch)),
        });
        handles().lock().unwrap().insert(id, h);
        id
    })
}
#[no_mangle]
pub extern "C" fn cs2w_destroy(id: u64) -> i32 {
    guarded(-4, || {
        if IN_CALLBACK.with(Cell::get) {
            return -3;
        }
        let Some(h) = handles().lock().unwrap().remove(&id) else {
            return -1;
        };
        // A standard thread may call this API even if its host thread has entered Tokio.
        let tx = h.tx.clone();
        let _ = thread::spawn(move || tx.blocking_send((0, Cmd::Quit))).join();
        if let Some(t) = h.worker.lock().unwrap().take() {
            let _ = t.join();
        }
        let _ = h.output.send(Dispatch::Quit);
        if let Some(t) = h.dispatch.lock().unwrap().take() {
            let _ = t.join();
        }
        0
    })
}
#[no_mangle]
pub unsafe extern "C" fn cs2w_start(id: u64, p: *const u8, n: usize) -> i64 {
    guarded(-4, || {
        unsafe { input(p, n) }.map_or_else(|e| e, |s| queue(id, Cmd::Start(s)))
    })
}
#[no_mangle]
pub unsafe extern "C" fn cs2w_reload(id: u64, p: *const u8, n: usize) -> i64 {
    guarded(-4, || {
        unsafe { input(p, n) }.map_or_else(|e| e, |s| queue(id, Cmd::Reload(s)))
    })
}
#[no_mangle]
pub extern "C" fn cs2w_stop(id: u64) -> i64 {
    guarded(-4, || queue(id, Cmd::Stop))
}
#[no_mangle]
pub extern "C" fn cs2w_preconnect(id: u64) -> i64 {
    guarded(-4, || queue(id, Cmd::Preconnect))
}
#[no_mangle]
pub extern "C" fn cs2w_start_desktop_tun(id: u64) -> i64 {
    guarded(-4, || queue(id, Cmd::DesktopTun))
}
#[no_mangle]
pub unsafe extern "C" fn cs2w_probe_a2s(id: u64, p: *const u8, n: usize, port: u16) -> i64 {
    guarded(-4, || {
        if port == 0 {
            return -2;
        }
        unsafe { input(p, n) }.map_or_else(|e| e, |s| queue(id, Cmd::Probe(s, port)))
    })
}
fn set(id: u64, log: bool, cb: Option<Callback>, user: *mut c_void) -> i32 {
    guarded(-4, || {
        if IN_CALLBACK.with(Cell::get) {
            return -3;
        }
        let Ok(h) = get(id) else { return -1 };
        let (tx, rx) = mpsc::channel();
        if h.output
            .send(Dispatch::Set(log, cb.map(|f| (f, user as usize)), tx))
            .is_err()
            || rx.recv().is_err()
        {
            return -4;
        }
        0
    })
}
#[no_mangle]
pub extern "C" fn cs2w_set_event_callback(id: u64, cb: Option<Callback>, ctx: *mut c_void) -> i32 {
    set(id, false, cb, ctx)
}
#[no_mangle]
pub extern "C" fn cs2w_set_log_callback(id: u64, cb: Option<Callback>, ctx: *mut c_void) -> i32 {
    set(id, true, cb, ctx)
}
#[repr(C)]
#[derive(Default)]
pub struct CStats {
    pub size: u32,
    pub version: u32,
    pub up_total: u64,
    pub down_total: u64,
    pub up_bps: u64,
    pub down_bps: u64,
    pub active_tcp: u64,
    pub active_udp: u64,
    pub wire_up: u64,
    pub wire_down: u64,
}
#[no_mangle]
pub unsafe extern "C" fn cs2w_get_stats(id: u64, out: *mut CStats) -> i32 {
    guarded(-4, || {
        if out.is_null() {
            return -2;
        }
        let p = out.cast::<u32>();
        if unsafe { p.read_unaligned() } < (std::mem::size_of::<CStats>() as u32)
            || unsafe { p.add(1).read_unaligned() } != 1
        {
            return -2;
        }
        let Ok(h) = get(id) else { return -1 };
        let s = *h.stats.lock().unwrap();
        unsafe {
            ptr::write_unaligned(
                out,
                CStats {
                    size: std::mem::size_of::<CStats>() as u32,
                    version: 1,
                    up_total: s.up_total,
                    down_total: s.down_total,
                    up_bps: s.up_bps,
                    down_bps: s.down_bps,
                    active_tcp: s.active_tcp,
                    active_udp: s.active_udp,
                    wire_up: s.wire_up,
                    wire_down: s.wire_down,
                },
            );
        }
        0
    })
}
