use bytes::{Buf, Bytes};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::PollSender;

pub(super) enum Command {
    Open(oneshot::Sender<io::Result<MuxStream>>),
    Data(u32, Vec<u8>),
    Fin(u32),
    Release(u32, bool),
}

pub(super) struct Chunk {
    data: Bytes,
    size: usize,
    charged: Arc<AtomicUsize>,
}
impl Chunk {
    pub(super) fn new(data: Bytes, charged: &Arc<AtomicUsize>) -> Self {
        let size = data.len();
        charged.fetch_add(size, Ordering::Relaxed);
        Self {
            data,
            size,
            charged: Arc::clone(charged),
        }
    }
}
impl Drop for Chunk {
    fn drop(&mut self) {
        self.charged.fetch_sub(self.size, Ordering::Relaxed);
    }
}

pub(super) type Inbox = mpsc::UnboundedSender<io::Result<Chunk>>;

/// A bidirectional smux v1 substream. Remote FIN closes only the read half.
pub struct MuxStream {
    id: u32,
    commands: mpsc::Sender<Command>,
    write: PollSender<Command>,
    received: mpsc::UnboundedReceiver<io::Result<Chunk>>,
    current: Option<Chunk>,
    closed: bool,
    runtime: Handle,
}

impl MuxStream {
    pub(super) fn new(id: u32, commands: mpsc::Sender<Command>) -> (Self, Inbox) {
        let (incoming, received) = mpsc::unbounded_channel();
        let write = PollSender::new(commands.clone());
        (
            Self {
                id,
                commands,
                write,
                received,
                current: None,
                closed: false,
                runtime: Handle::current(),
            },
            incoming,
        )
    }
}
fn broken() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "CS2World session closed")
}

impl AsyncRead for MuxStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        dst: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if dst.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.current.is_none() {
            match ready!(self.received.poll_recv(cx)) {
                Some(Ok(chunk)) => self.current = Some(chunk),
                Some(Err(err)) => return Poll::Ready(Err(err)),
                None => return Poll::Ready(Ok(())),
            }
        }
        let chunk = self.current.as_mut().expect("current read chunk");
        let n = dst.remaining().min(chunk.data.len());
        dst.put_slice(&chunk.data[..n]);
        chunk.data.advance(n);
        if chunk.data.is_empty() {
            self.current = None;
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for MuxStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed {
            return Poll::Ready(Err(broken()));
        }
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        ready!(self.write.poll_reserve(cx)).map_err(|_| broken())?;
        let n = data.len().min(cs2world_proto::mux::MAX_FRAME);
        let id = self.id;
        self.write
            .send_item(Command::Data(id, data[..n].to_vec()))
            .map_err(|_| broken())?;
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // poll_write has already passed the bytes to the ordered session queue.
        Poll::Ready(if self.commands.is_closed() {
            Err(broken())
        } else {
            Ok(())
        })
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.closed {
            ready!(self.write.poll_reserve(cx)).map_err(|_| broken())?;
            let id = self.id;
            self.write
                .send_item(Command::Fin(id))
                .map_err(|_| broken())?;
            self.closed = true;
        }
        Poll::Ready(Ok(()))
    }
}
impl Drop for MuxStream {
    fn drop(&mut self) {
        self.write.abort_send();
        let release = Command::Release(self.id, !self.closed);
        if let Err(mpsc::error::TrySendError::Full(release)) = self.commands.try_send(release) {
            let commands = self.commands.clone();
            self.runtime.spawn(async move {
                let _ = commands.send(release).await;
            });
        }
    }
}
