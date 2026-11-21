use super::{Bridge, TunSession};
use crate::platform_socket::desktop::set_interfaces;
use route_manager::{AsyncRouteManager as Handle, Route};
use std::{
    io,
    net::{IpAddr, SocketAddr},
};
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

static DESKTOP: Semaphore = Semaphore::const_new(1);

/// Adds exact endpoint exclusions and split defaults. Each successful add is
/// journaled before the next operation so partial startup can roll back.
struct Routes {
    handle: Handle,
    original: Vec<Route>,
    added: Vec<Route>,
}
impl Routes {
    async fn snapshot() -> io::Result<Self> {
        let mut handle = Handle::new()?;
        let original = handle.list().await?;
        Ok(Self {
            handle,
            original,
            added: Vec::new(),
        })
    }
    fn best(&self, ip: IpAddr) -> Option<&Route> {
        self.original
            .iter()
            .filter(|r| r.if_index().is_some() && r.contains(&ip))
            .min_by_key(|r| (u8::MAX - r.prefix(), metric(r)))
    }
    async fn add(&mut self, route: Route) -> io::Result<()> {
        self.handle.add(&route).await?;
        self.added.push(route);
        Ok(())
    }
    async fn install(
        &mut self,
        idx: u32,
        node: IpAddr,
        prefixes: &[ipnet::IpNet],
    ) -> io::Result<()> {
        let v4 = self
            .original
            .iter()
            .filter(|r| r.prefix() == 0 && r.destination().is_ipv4())
            .min_by_key(|r| metric(r))
            .and_then(|r| r.if_index());
        let v6 = self
            .original
            .iter()
            .filter(|r| r.prefix() == 0 && r.destination().is_ipv6())
            .min_by_key(|r| metric(r))
            .and_then(|r| r.if_index());
        if !node.is_loopback() && prefixes.iter().any(|p| p.contains(&node)) {
            let best = self.best(node).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NetworkUnreachable,
                    "no physical route to node",
                )
            })?;
            if best.prefix() <= 1 {
                let mut host = Route::new(node, if node.is_ipv4() { 32 } else { 128 })
                    .with_if_index(best.if_index().unwrap());
                if let Some(gateway) = best.gateway() {
                    host = host.with_gateway(gateway);
                }
                self.add(host).await?;
            }
        }
        // If one family has no physical default route, keep sockets of that
        // family on loopback. An unbound socket could feed its own packets
        // back into the new split default.
        let fallback = |ip| self.best(ip).and_then(Route::if_index).unwrap_or(u32::MAX);
        set_interfaces(
            v4.unwrap_or_else(|| fallback(std::net::Ipv4Addr::LOCALHOST.into())),
            v6.unwrap_or_else(|| fallback(std::net::Ipv6Addr::LOCALHOST.into())),
        );
        for prefix in prefixes {
            let ip = prefix.network();
            let bits = prefix.prefix_len();
            if self
                .original
                .iter()
                .any(|r| r.destination() == ip && r.prefix() == bits)
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "another capture route is active",
                ));
            }
            self.add(Route::new(ip, bits).with_if_index(idx)).await?;
        }
        Ok(())
    }
    async fn restore(&mut self) -> io::Result<()> {
        let mut last = None;
        while let Some(route) = self.added.pop() {
            if let Err(e) = self.handle.delete(&route).await {
                last = Some(e);
            }
        }
        set_interfaces(0, 0);
        last.map_or(Ok(()), Err)
    }
}
fn metric(r: &Route) -> u32 {
    #[cfg(windows)]
    {
        r.metric().unwrap_or(0)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = r;
        0
    }
}

#[cfg(windows)]
struct Driver(tempfile::TempDir);
#[cfg(windows)]
impl Driver {
    fn prepare() -> io::Result<Self> {
        use std::{fs::OpenOptions, io::Write};
        #[cfg(target_arch = "x86_64")]
        const DLL: &[u8] = include_bytes!("../../vendor/wintun/bin/amd64/wintun.dll");
        #[cfg(target_arch = "aarch64")]
        const DLL: &[u8] = include_bytes!("../../vendor/wintun/bin/arm64/wintun.dll");
        #[cfg(target_arch = "x86")]
        const DLL: &[u8] = include_bytes!("../../vendor/wintun/bin/x86/wintun.dll");
        #[cfg(target_arch = "arm")]
        const DLL: &[u8] = include_bytes!("../../vendor/wintun/bin/arm/wintun.dll");
        // The driver runs with elevated privileges. Extract below a machine-owned
        // directory rather than inheriting the caller's writable TEMP directory.
        use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
        use windows_sys::Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath},
        };
        let mut path = std::ptr::null_mut();
        let status = unsafe {
            SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut path)
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "cannot locate Program Files (HRESULT {status:#x})"
            )));
        }
        // SHGetKnownFolderPath returns a NUL-terminated, COM-allocated UTF-16 string.
        let root = unsafe {
            let mut n = 0;
            while *path.add(n) != 0 {
                n += 1;
            }
            let root = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(path, n)));
            CoTaskMemFree(path.cast());
            root
        };
        let dir = tempfile::Builder::new()
            .prefix("CS2World-Wintun-")
            .tempdir_in(root)?;
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.path().join("wintun.dll"))?;
        f.write_all(DLL)?;
        std::fs::write(
            dir.path().join("LICENSE.txt"),
            include_bytes!("../../vendor/wintun/LICENSE.txt"),
        )?;
        Ok(Self(dir))
    }
}
pub async fn start_desktop(mixed: SocketAddr, node_endpoint: SocketAddr) -> io::Result<TunSession> {
    let capture = ["0.0.0.0/1", "128.0.0.0/1", "::/1", "8000::/1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    start_capture(mixed, node_endpoint, capture).await
}
async fn start_capture(
    mixed: SocketAddr,
    node_endpoint: SocketAddr,
    capture: Vec<ipnet::IpNet>,
) -> io::Result<TunSession> {
    let lease = DESKTOP.try_acquire().map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a desktop TUN is already active",
        )
    })?;
    let bridge = Bridge::new(mixed)?;
    let mut routes = Routes::snapshot().await?;
    let cancel = CancellationToken::new();
    let worker = cancel.clone();
    let (ready, started) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _lease = lease;
        #[cfg(windows)]
        let driver = std::sync::Arc::new(Driver::prepare()?);
        #[cfg(windows)]
        let dll = driver.clone();
        let device = tokio::task::spawn_blocking(move || {
            let builder = tun_rs::DeviceBuilder::new()
                .mtu(1500)
                .ipv4("172.31.255.1", 30, Some("172.31.255.2"))
                .ipv6("fd42:6373:3277::1", 126);
            #[cfg(windows)]
            let builder = builder.name("CS2World").wintun_file(
                dll.0
                    .path()
                    .join("wintun.dll")
                    .to_string_lossy()
                    .into_owned(),
            );
            builder.build_sync()
        })
        .await
        .map_err(io::Error::other)??;
        let idx = device.if_index()?;
        let device = std::sync::Arc::new(tun_rs::AsyncDevice::new(device)?);
        let result = async {
            routes.install(idx, node_endpoint.ip(), &capture).await?;
            if ready.send(()).is_err() {
                return Ok(());
            }
            bridge.drive(device.clone(), worker).await
        }
        .await;
        let cleanup = routes.restore().await;
        result.and(cleanup)
    });
    let mut session = TunSession {
        cancel,
        task: Some(task),
    };
    if started.await.is_err() {
        session.stop().await?;
        return Err(io::Error::other("TUN stopped before startup"));
    }
    Ok(session)
}

#[cfg(test)]
mod tests;
