use std::io;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;

pub(super) fn now_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}
pub(super) fn unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub(super) fn local_hour() -> u8 {
    #[cfg(unix)]
    {
        let t = unix() as libc::time_t;
        let mut out = std::mem::MaybeUninit::uninit();
        // localtime_r fills our own tm, without borrowing the process-wide time buffer.
        if !unsafe { libc::localtime_r(&t, out.as_mut_ptr()) }.is_null() {
            return unsafe { out.assume_init() }.tm_hour as u8;
        }
    }
    #[cfg(windows)]
    {
        let mut out = std::mem::MaybeUninit::uninit();
        unsafe {
            windows_sys::Win32::System::SystemInformation::GetLocalTime(out.as_mut_ptr());
            return out.assume_init().wHour as u8;
        }
    }
    #[cfg(not(windows))]
    {
        ((unix() / 3600) % 24) as u8
    }
}

pub(super) fn udp(bind: SocketAddr) -> io::Result<UdpSocket> {
    let family = if bind.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(family, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    let _ = socket.set_recv_buffer_size(4 * 1024 * 1024);
    let _ = socket.set_send_buffer_size(4 * 1024 * 1024);
    socket.set_nonblocking(true)?;
    socket.bind(&bind.into())?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::WSAIoctl;
        let off = 0u32;
        let mut returned = 0u32;
        unsafe {
            WSAIoctl(
                socket.as_raw_socket() as _,
                0x9800000c,
                (&off as *const u32).cast(),
                4,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
                None,
            );
        }
    }
    UdpSocket::from_std(socket.into())
}
pub(super) fn transient(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(e.raw_os_error(), Some(10054 | 10040))
    }
    #[cfg(not(windows))]
    {
        e.kind() == io::ErrorKind::Interrupted
    }
}
