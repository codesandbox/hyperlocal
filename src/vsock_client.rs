use hex::FromHex;
use hyper::{body::Body, rt::ReadBufCursor, Uri};
use hyper_util::{
    client::legacy::{
        connect::{Connected, Connection},
        Client,
    },
    rt::{TokioExecutor, TokioIo},
};
use pin_project_lite::pin_project;
use std::{
    future::Future,
    io,
    io::Error,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, Interest, ReadBuf};
use tower_service::Service;

pin_project! {
    #[derive(Debug)]
    pub struct VsockUnixStream {
        #[pin]
        unix_stream: tokio::net::UnixStream,
        port: u32,
    }
}

impl VsockUnixStream {
    async fn connect<P>(
        path: P,
        port: u32,
    ) -> std::io::Result<Self>
    where
        P: AsRef<Path>,
    {
        let mut unix_stream = tokio::net::UnixStream::connect(path).await?;
        unix_stream.ready(Interest::WRITABLE).await?;

        let connect_command = format!("CONNECT {port}\n").into_bytes();
        unix_stream.try_write(&connect_command)?;

        unix_stream.ready(Interest::READABLE).await?;

        // Read the exact response, being 'OK 1073741824\n' where the number is the opened port
        let mut response: [u8; 14] = [0; 14];
        unix_stream.read_exact(&mut response).await?;

        // Check for the first 2 bytes to be "O" and "K" ("OK").
        if response[0] == 79 && response[1] == 75 {
            Ok(Self { unix_stream, port })
        } else {
            Err(io::ErrorKind::InvalidData.into())
        }
    }
}

impl AsyncWrite for VsockUnixStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.project().unix_stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.project().unix_stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.project().unix_stream.poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, Error>> {
        self.project().unix_stream.poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.unix_stream.is_write_vectored()
    }
}

impl hyper::rt::Write for VsockUnixStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        self.project().unix_stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        self.project().unix_stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        self.project().unix_stream.poll_shutdown(cx)
    }
}

impl AsyncRead for VsockUnixStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().unix_stream.poll_read(cx, buf)
    }
}

impl hyper::rt::Read for VsockUnixStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<Result<(), Error>> {
        let mut t = TokioIo::new(self.project().unix_stream);
        Pin::new(&mut t).poll_read(cx, buf)
    }
}

/// the `[VsockUnixConnector]` can be used to construct a `[hyper::Client]` which can
/// speak to a unix domain socket.
///
/// # Example
/// ```
/// use http_body_util::Full;
/// use hyper::body::Bytes;
/// use hyper_util::{client::legacy::Client, rt::TokioExecutor};
/// use hyperlocal::VsockUnixConnector;
///
/// let connector = VsockUnixConnector;
/// let client: Client<VsockUnixConnector, Full<Bytes>> =
///     Client::builder(TokioExecutor::new()).build(connector);
/// ```
///
/// # Note
/// If you don't need access to the low-level `[hyper::Client]` builder
/// interface, consider using the `[UnixClientExt]` trait instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct VsockUnixConnector {
    /// The vsock port to connect to
    port: u32,
}

impl Unpin for VsockUnixConnector {}

impl Service<Uri> for VsockUnixConnector {
    type Response = VsockUnixStream;
    type Error = io::Error;
    #[allow(clippy::type_complexity)]
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn call(
        &mut self,
        req: Uri,
    ) -> Self::Future {
        let port = self.port;
        let fut = async move {
            let path = parse_socket_path(&req)?;
            VsockUnixStream::connect(path, port).await
        };

        Box::pin(fut)
    }

    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Connection for VsockUnixStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

fn parse_socket_path(uri: &Uri) -> Result<PathBuf, io::Error> {
    if uri.scheme_str() != Some("unix") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid URL, scheme must be unix",
        ));
    }

    if let Some(host) = uri.host() {
        let bytes = Vec::from_hex(host).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid URL, host must be a hex-encoded path",
            )
        })?;

        Ok(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid URL, host must be present",
        ))
    }
}

/// Extension trait for constructing a hyper HTTP client over a Unix domain
/// socket.
pub trait VsockUnixClientExt<B: Body + Send> {
    /// Construct a client which speaks HTTP over a Unix domain socket
    ///
    /// # Example
    /// ```
    /// use http_body_util::Full;
    /// use hyper::body::Bytes;
    /// use hyper_util::client::legacy::Client;
    /// use hyperlocal::{VsockClientExt, VsockUnixConnector};
    ///
    /// let client: Client<VsockUnixConnector, Full<Bytes>> = Client::unix();
    /// ```
    #[must_use]
    fn vsock(port: u32) -> Client<VsockUnixConnector, B>
    where
        B::Data: Send,
    {
        Client::builder(TokioExecutor::new()).build(VsockUnixConnector { port })
    }
}

impl<B: Body + Send> VsockUnixClientExt<B> for Client<VsockUnixConnector, B> {}
