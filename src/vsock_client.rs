use futures_util::future::BoxFuture;
use hex::FromHex;
use hyper::{
    client::connect::{Connected, Connection},
    service::Service,
    Body, Client, Uri,
};
use pin_project::pin_project;
use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncReadExt, Interest, ReadBuf};

#[pin_project]
#[derive(Debug)]
pub struct VsockUnixStream {
    #[pin]
    unix_stream: tokio::net::UnixStream,

    port: u32,
}

impl VsockUnixStream {
    async fn connect<P>(path: P, port: u32) -> std::io::Result<Self>
    where
        P: AsRef<Path>,
    {
        let mut unix_stream = tokio::net::UnixStream::connect(path).await?;
        unix_stream.ready(Interest::WRITABLE).await?;

        let connect_command = format!("CONNECT {}\n", port).into_bytes();
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

impl tokio::io::AsyncWrite for VsockUnixStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.project().unix_stream.poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().unix_stream.poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().unix_stream.poll_shutdown(cx)
    }
}

impl tokio::io::AsyncRead for VsockUnixStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().unix_stream.poll_read(cx, buf)
    }
}

/// the `[VsockUnixConnector]` can be used to construct a `[hyper::Client]` which can
/// speak to a unix domain socket.
///
/// # Example
/// ```
/// use hyper::{Client, Body};
/// use hyperlocal::VsockUnixConnector;
///
/// let connector = VsockUnixConnector { port: 10000 };
/// let client: Client<VsockUnixConnector, Body> = Client::builder().build(connector);
/// ```
///
/// # Note
/// If you don't need access to the low-level `[hyper::Client]` builder
/// interface, consider using the `[VsockUnixClientExt]` trait instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct VsockUnixConnector {
    /// The vsock port to connect to
    pub port: u32,
}

impl Unpin for VsockUnixConnector {}

impl Service<Uri> for VsockUnixConnector {
    type Response = VsockUnixStream;
    type Error = std::io::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;
    fn call(&mut self, req: Uri) -> Self::Future {
        let port = self.port;
        let fut = async move {
            let path = parse_socket_path(req)?;
            VsockUnixStream::connect(path, port).await
        };

        Box::pin(fut)
    }
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Connection for VsockUnixStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

fn parse_socket_path(uri: Uri) -> Result<std::path::PathBuf, io::Error> {
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

/// Extention trait for constructing a hyper HTTP client over a VsockUnix domain
/// socket.
pub trait VsockUnixClientExt {
    /// Construct a client which speaks HTTP over a VsockUnix domain socket
    ///
    /// # Example
    /// ```
    /// use hyper::Client;
    /// use hyperlocal::VsockUnixClientExt;
    ///
    /// let client = Client::vsock(10000);
    /// ```
    fn vsock(port: u32) -> Client<VsockUnixConnector, Body> {
        Client::builder().build(VsockUnixConnector { port })
    }
}

impl VsockUnixClientExt for Client<VsockUnixConnector> {}
