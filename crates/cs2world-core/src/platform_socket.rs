//! Physical-network socket selection for the VPN transport and direct outbounds.
use std::{io, net::SocketAddr};
use tokio::net::{TcpSocket, TcpStream, UdpSocket};

#[cfg(target_os = "android")]
mod android {
    use std::{
        io,
        os::fd::AsRawFd,
        sync::{Arc, RwLock},
    };
    pub type Protector = Arc<dyn Fn(i32) -> bool + Send + Sync>;
    static PROTECTOR: RwLock<Option<Protector>> = RwLock::new(None);
    pub fn set_protector(value: Option<Protector>) {
        *PROTECTOR.write().unwrap() = value;
    }
    pub fn protect(s: &impl AsRawFd) -> io::Result<()> {
        let callback = PROTECTOR.read().unwrap().clone();
        if callback.is_some_and(|f| !f(s.as_raw_fd())) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VpnService.protect refused socket",
            ));
        }
        Ok(())
    }
}
#[cfg(target_os = "android")]
pub use android::{protect, set_protector};

#[cfg(all(feature = "tun", any(windows, target_os = "macos")))]
pub(crate) mod desktop {
    use std::io;
    use std::sync::atomic::{AtomicU32, Ordering};
    static IPV4: AtomicU32 = AtomicU32::new(0);
    static IPV6: AtomicU32 = AtomicU32::new(0);
    pub fn set_interfaces(ipv4: u32, ipv6: u32) {
        IPV4.store(ipv4, Ordering::SeqCst);
        IPV6.store(ipv6, Ordering::SeqCst);
    }
    pub fn bind(sock: socket2::SockRef<'_>, v4: bool) -> io::Result<()> {
        let idx = if v4 {
            IPV4.load(Ordering::SeqCst)
        } else {
            IPV6.load(Ordering::SeqCst)
        };
        if idx == 0 {
            return Ok(());
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            use windows_sys::Win32::Networking::WinSock::{
                setsockopt, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, IP_UNICAST_IF,
            };
            let (level, option, value) = if v4 {
                (IPPROTO_IP, IP_UNICAST_IF, idx.to_be())
            } else {
                (IPPROTO_IPV6, IPV6_UNICAST_IF, idx)
            };
            if unsafe {
                setsockopt(
                    sock.as_raw_socket() as _,
                    level,
                    option,
                    (&value as *const u32).cast(),
                    4,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            let (level, opt) = if v4 {
                (libc::IPPROTO_IP, libc::IP_BOUND_IF)
            } else {
                (libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF)
            };
            if unsafe {
                libc::setsockopt(sock.as_raw_fd(), level, opt, (&idx as *const u32).cast(), 4)
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
pub(crate) fn bind_udp_interface(socket: &UdpSocket) -> io::Result<()> {
    #[cfg(target_os = "android")]
    protect(socket)?;
    #[cfg(all(feature = "tun", any(windows, target_os = "macos")))]
    desktop::bind(
        socket2::SockRef::from(socket),
        socket.local_addr()?.is_ipv4(),
    )?;
    let _ = socket;
    Ok(())
}
pub async fn bind_udp(addr: SocketAddr) -> io::Result<UdpSocket> {
    let s = UdpSocket::bind(addr).await?;
    bind_udp_interface(&s)?;
    Ok(s)
}
pub async fn connect_tcp(addr: SocketAddr) -> io::Result<TcpStream> {
    if addr.ip().is_loopback() {
        return TcpStream::connect(addr).await;
    }
    let s = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    #[cfg(target_os = "android")]
    protect(&s)?;
    #[cfg(all(feature = "tun", any(windows, target_os = "macos")))]
    desktop::bind(socket2::SockRef::from(&s), addr.is_ipv4())?;
    s.connect(addr).await
}
