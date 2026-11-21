use crate::models::{AppSettings, Cs2Node, PersistedState, TrafficSnapshot};
use crate::{singbox_config::core_config, sysproxy::SystemProxy};
use cs2world_core::{service::Service, tun::TunSession};
use std::{
    collections::VecDeque,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc, watch};
use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::PostMessageW};

#[derive(Clone)]
enum Intent {
    Stop,
    Connect(Box<(Cs2Node, AppSettings)>),
    Quit,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Stopped,
    Starting,
    Listening,
    Running,
    Stopping,
    Failed,
}
#[derive(Default)]
struct Runtime {
    status: Status,
    started: Option<Instant>,
    error: Option<String>,
    traffic: TrafficSnapshot,
}
pub struct CoreController {
    memory: Mutex<Option<PersistedState>>,
    runtime: Mutex<Runtime>,
    logs: Mutex<VecDeque<String>>,
    intent: watch::Sender<Intent>,
    save: mpsc::UnboundedSender<PersistedState>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    hwnd: usize,
}
fn storage() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_owned))
        .unwrap_or_default()
        .join("data")
        .join("state.json")
}
pub fn read_state(path: &Path) -> io::Result<PersistedState> {
    let data = fs::read(path)?;
    serde_json::from_slice(&data)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "节点文件格式无效"))
}
fn save_state(path: &Path, state: &PersistedState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}
pub fn selected(state: &PersistedState) -> Option<&Cs2Node> {
    match state.settings.active_node_id.as_deref() {
        Some(id) => state.nodes.iter().find(|n| n.id == id),
        None => state.nodes.first(),
    }
}
impl CoreController {
    pub fn new(hwnd: usize) -> Arc<Self> {
        let (intent, rx) = watch::channel(Intent::Stop);
        let (save, saves) = mpsc::unbounded_channel();
        let c = Arc::new(Self {
            memory: Mutex::new(None),
            runtime: Mutex::new(Runtime::default()),
            logs: Mutex::new(VecDeque::new()),
            intent,
            save,
            worker: Mutex::new(None),
            hwnd,
        });
        let actor = c.clone();
        let handle = thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            match rt {
                Ok(rt) => rt.block_on(actor.run(rx, saves)),
                Err(_) => actor.fail("无法启动内嵌网络运行时"),
            }
        });
        *c.worker.lock().unwrap() = Some(handle);
        c
    }
    fn notify(&self, kind: u32) {
        unsafe {
            PostMessageW(self.hwnd as HWND, kind, 0, 0);
        }
    }
    pub fn ready(&self) -> bool {
        self.memory.lock().unwrap().is_some()
    }
    pub fn get_persisted(&self) -> PersistedState {
        self.memory.lock().unwrap().clone().unwrap_or_default()
    }
    pub fn mutate_persisted<F, R>(&self, update: F) -> Result<R, String>
    where
        F: FnOnce(&mut PersistedState) -> R,
    {
        let mut guard = self.memory.lock().unwrap();
        let s = guard.as_mut().ok_or("节点文件尚未载入")?;
        let value = update(s);
        self.save.send(s.clone()).map_err(|_| "后台存储已关闭")?;
        Ok(value)
    }
    pub fn active_node(&self) -> Option<Cs2Node> {
        selected(self.memory.lock().unwrap().as_ref()?).cloned()
    }
    pub fn status(&self) -> Status {
        self.runtime.lock().unwrap().status
    }
    pub fn is_connected(&self) -> bool {
        self.status() == Status::Running
    }
    pub fn desired(&self) -> bool {
        matches!(&*self.intent.borrow(), Intent::Connect(_))
    }
    pub fn uptime_secs(&self) -> u64 {
        self.runtime
            .lock()
            .unwrap()
            .started
            .map_or(0, |t| t.elapsed().as_secs())
    }
    pub fn last_error(&self) -> Option<String> {
        self.runtime.lock().unwrap().error.clone()
    }
    pub fn poll_traffic(&self) -> TrafficSnapshot {
        self.runtime.lock().unwrap().traffic.clone()
    }
    pub fn push_log(&self, message: &str) {
        let mut logs = self.logs.lock().unwrap();
        if logs.len() >= 400 {
            logs.pop_front();
        }
        logs.push_back(message.to_owned());
    }
    pub fn get_logs_text(&self) -> String {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\r\n")
    }
    pub fn clear_logs(&self) {
        self.logs.lock().unwrap().clear();
    }
    fn status_change(&self, status: Status) {
        let mut rt = self.runtime.lock().unwrap();
        rt.status = status;
        rt.error = None;
        if status == Status::Running {
            rt.started = Some(Instant::now());
        }
        if status == Status::Stopped {
            rt.started = None;
            rt.traffic = TrafficSnapshot::default();
        }
        drop(rt);
        self.notify(crate::WM_CORE_STATUS);
    }
    fn fail(&self, msg: &str) {
        let mut rt = self.runtime.lock().unwrap();
        rt.error = Some(msg.into());
        rt.status = Status::Failed;
        drop(rt);
        self.push_log(msg);
        self.notify(crate::WM_CORE_STATUS);
    }
    pub fn connect(&self) -> Result<(), String> {
        let state = self.get_persisted();
        let node = selected(&state).ok_or("请先选择有效节点")?.clone();
        self.intent
            .send(Intent::Connect(Box::new((node, state.settings))))
            .map_err(|_| "后台网络运行时已停止".to_string())
    }
    pub fn disconnect(&self) {
        let _ = self.intent.send(Intent::Stop);
    }
    pub fn shutdown(&self) {
        let handle = self.worker.lock().unwrap();
        if handle.as_ref().is_none_or(thread::JoinHandle::is_finished)
            || self.intent.send(Intent::Quit).is_err()
        {
            self.notify(crate::WM_CORE_EXITED);
        }
    }
    pub fn join(&self) {
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
    pub fn apply_settings(&self, s: AppSettings) -> Result<(), String> {
        if s.mixed_port == 0 {
            return Err("本地端口必须在 1–65535 之间".into());
        }
        self.mutate_persisted(|state| state.settings = s)?;
        if self.desired() {
            self.connect()?;
        }
        Ok(())
    }

    async fn run(
        self: &Arc<Self>,
        mut intents: watch::Receiver<Intent>,
        mut saves: mpsc::UnboundedReceiver<PersistedState>,
    ) {
        let path = storage();
        match read_state(&path) {
            Ok(state) => *self.memory.lock().unwrap() = Some(state),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                *self.memory.lock().unwrap() = Some(PersistedState::default())
            }
            Err(_) => self.fail("原节点文件无法加载，已保留原文件。"),
        }
        self.notify(crate::WM_CORE_LOADED);
        let io_owner = self.clone();
        let (finish_tx, mut finish_rx) = watch::channel(false);
        let writer = tokio::spawn(async move {
            loop {
                let next =
                    tokio::select! {biased; data=saves.recv()=>data, _=finish_rx.changed()=>None};
                let Some(mut latest) = next else {
                    break;
                };
                while let Ok(newer) = saves.try_recv() {
                    latest = newer;
                }
                if save_state(&path, &latest).is_err() {
                    io_owner.push_log("保存节点文件失败，请检查目录写权限。");
                    io_owner.notify(crate::WM_CORE_STATUS);
                }
            }
        });
        let mut session = Running::default();
        let mut active: Option<(Cs2Node, AppSettings)> = None;
        let mut sample = tokio::time::interval(Duration::from_millis(300));
        let mut health = tokio::time::interval(Duration::from_secs(10));
        health.tick().await;
        let mut retry: Option<Instant> = None;
        let mut backoff = Duration::from_secs(2);
        let mut apply = false;
        loop {
            if apply {
                apply = false;
                let next = intents.borrow_and_update().clone();
                match next {
                    Intent::Quit => break,
                    Intent::Stop => {
                        retry = None;
                        active = None;
                        self.status_change(Status::Stopping);
                        session.stop(self).await;
                        self.status_change(Status::Stopped);
                    }
                    Intent::Connect(request) => {
                        self.status_change(Status::Starting);
                        let (node, settings) = *request;
                        let result = tokio::select! {
                            biased;
                            changed=intents.changed()=>{if changed.is_err(){break;}apply=true;continue;},
                            outcome=session.configure(self,&node,&settings)=>outcome,
                        };
                        match result {
                            Ok(()) => {
                                retry = None;
                                backoff = Duration::from_secs(2);
                                self.push_log("节点认证成功。");
                                self.status_change(Status::Running);
                                active = Some((node, settings));
                            }
                            Err(e) => {
                                if let Some((old_node, old_settings)) = &active {
                                    let restored = tokio::select! {
                                        biased;
                                        changed=intents.changed()=>{if changed.is_err(){break;}apply=true;continue;},
                                        r=session.configure(self,old_node,old_settings)=>r.is_ok(),
                                    };
                                    if restored {
                                        self.push_log("新配置未生效，已恢复先前连接。");
                                        let _ = self.mutate_persisted(|p| {
                                            p.settings = old_settings.clone()
                                        });
                                        self.status_change(Status::Running);
                                        self.notify(crate::WM_CORE_LOADED);
                                        continue;
                                    }
                                }
                                session.stop(self).await;
                                active = None;
                                let msg = match e.kind() {
                                    io::ErrorKind::TimedOut => {
                                        "节点握手超时，请检查网络和节点地址。"
                                    }
                                    io::ErrorKind::PermissionDenied => {
                                        "请求被拒绝，请检查密钥、时钟或系统权限。"
                                    }
                                    io::ErrorKind::AddrInUse => "本地端口已占用。",
                                    _ => "连接失败，请检查节点、端口或 TUN 驱动。",
                                };
                                self.fail(msg);
                                if matches!(
                                    e.kind(),
                                    io::ErrorKind::TimedOut
                                        | io::ErrorKind::NetworkUnreachable
                                        | io::ErrorKind::NotConnected
                                        | io::ErrorKind::ConnectionReset
                                ) {
                                    retry = Some(Instant::now() + backoff);
                                    backoff = (backoff * 2).min(Duration::from_secs(30));
                                }
                            }
                        }
                    }
                }
            }
            tokio::select! {
                r=intents.changed()=>{if r.is_err(){break;}retry=None;backoff=Duration::from_secs(2);apply=true;}
                _=sample.tick()=>{
                    session.sample(self);
                    if retry.is_some_and(|time|Instant::now()>=time){retry=None;apply=true;}
                }
                _=health.tick(),if active.is_some()=>{
                    if let Some(service)=&session.service {
                        tokio::select!{
                            biased;
                            r=intents.changed()=>{if r.is_err(){break;}apply=true;}
                            outcome=service.preconnect()=>{
                                if outcome.is_err(){
                                    self.push_log("隧道连接中断，正在重新连接。");
                                    active=None;
                                    apply=true;
                                }
                            }
                        }
                    }
                }
            }
        }
        session.stop(self).await;
        finish_tx.send_replace(true);
        let _ = writer.await;
        self.notify(crate::WM_CORE_EXITED);
    }
}
#[derive(Default)]
struct Running {
    service: Option<Service>,
    tun: Option<TunSession>,
    proxy: SystemProxy,
    logs: Option<broadcast::Receiver<String>>,
}
impl Running {
    async fn configure(
        &mut self,
        owner: &CoreController,
        node: &Cs2Node,
        settings: &AppSettings,
    ) -> io::Result<()> {
        let config = core_config(node, settings)?;
        if let Some(s) = &mut self.service {
            s.reload(config).await?;
        } else {
            self.service = Some(Service::start(config).await?);
        }
        let service = self.service.as_ref().expect("started");
        self.logs = Some(service.subscribe());
        owner.status_change(Status::Listening);
        service.preconnect().await?;
        let mixed = service.local_addr();
        if let Some(mut tun) = self.tun.take() {
            tun.stop().await?;
        }
        if settings.proxy_mode == "tun" {
            self.proxy.disable()?;
            let endpoint = tokio::net::lookup_host((node.server.as_str(), node.server_port))
                .await?
                .next()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::AddrNotAvailable, "unresolved node")
                })?;
            self.tun = Some(cs2world_core::tun::start_desktop(mixed, endpoint).await?);
        } else if settings.proxy_mode == "system" {
            self.proxy.enable(mixed.port())?;
        } else {
            self.proxy.disable()?;
        }
        Ok(())
    }
    fn sample(&mut self, owner: &CoreController) {
        if let Some(s) = &self.service {
            let m = s.snapshot();
            owner.runtime.lock().unwrap().traffic = TrafficSnapshot {
                up_bps: m.up_bps,
                down_bps: m.down_bps,
                up_total: m.up_total,
                down_total: m.down_total,
                active_connections: (m.active_tcp + m.active_udp) as usize,
            };
        }
        if let Some(rx) = &mut self.logs {
            loop {
                match rx.try_recv() {
                    Ok(s) => owner.push_log(&s),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    async fn stop(&mut self, owner: &CoreController) {
        if self.proxy.disable().is_err() {
            owner.push_log("还原系统代理失败，退出时将重试。");
        }
        if let Some(mut t) = self.tun.take() {
            if t.stop().await.is_err() {
                owner.push_log("撤销 TUN 路由失败。");
            }
        }
        if let Some(mut s) = self.service.take() {
            s.stop().await;
        }
        self.logs = None;
    }
}
