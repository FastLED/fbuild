//! Neutral fbuild-owned IPC endpoint and peer APIs.

use std::io::{Read, Write};
use std::time::Duration;

/// Host-neutral facts about the process connected to a local endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerFacts {
    pub pid: Option<u32>,
    pub user_id: Option<u32>,
    pub group_id: Option<u32>,
}

/// Failure while constructing one native TCP listener.
#[derive(Debug, thiserror::Error)]
pub enum TcpListenerError {
    /// The endpoint is currently unavailable; daemon policy may probe/retry it.
    #[error(transparent)]
    Bind(std::io::Error),
    /// Listener construction failed outside the retryable bind operation.
    #[error("failed to {operation}: {source}")]
    Setup {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl TcpListenerError {
    pub(crate) fn setup(operation: &'static str, source: std::io::Error) -> Self {
        Self::Setup { operation, source }
    }
}

/// An fbuild-owned local endpoint listener.
pub struct LocalListener {
    inner: super::selected::ipc::LocalListener,
}

impl LocalListener {
    /// Bind an owner-private local endpoint using the host's native transport.
    pub fn bind(endpoint: &str) -> std::io::Result<Self> {
        super::selected::ipc::bind_local(endpoint).map(|inner| Self { inner })
    }

    pub fn accept(&self) -> std::io::Result<LocalStream> {
        super::selected::ipc::accept(&self.inner).map(|inner| LocalStream { inner })
    }

    pub fn incoming(&self) -> Incoming<'_> {
        Incoming { listener: self }
    }
}

/// Infinite iterator over clients accepted by a [`LocalListener`].
pub struct Incoming<'a> {
    listener: &'a LocalListener,
}

impl Iterator for Incoming<'_> {
    type Item = std::io::Result<LocalStream>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.listener.accept())
    }
}

/// A byte stream connected to an fbuild-owned local endpoint.
pub struct LocalStream {
    inner: super::selected::ipc::LocalStream,
}

impl LocalStream {
    pub fn peer_facts(&self) -> std::io::Result<PeerFacts> {
        super::selected::ipc::peer_facts(&self.inner)
    }
}

impl Read for LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Write for LocalStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Connect to an fbuild-owned local endpoint.
pub fn connect(endpoint: &str) -> std::io::Result<LocalStream> {
    super::selected::ipc::connect_local(endpoint).map(|inner| LocalStream { inner })
}

/// Build one host-configured TCP listener. Retry and ownership policy remain
/// with the daemon caller.
pub fn bind_tcp_listener(
    address: std::net::SocketAddr,
) -> Result<tokio::net::TcpListener, TcpListenerError> {
    super::selected::ipc::bind_tcp_listener(address)
}

/// Return whether a TCP endpoint accepts a connection within `timeout`.
pub async fn tcp_endpoint_ready(address: std::net::SocketAddr, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect(address)).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    fn unique_endpoint() -> String {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        if crate::platform::host::is_windows() {
            format!("fbuild-platform-ipc-{}-{nonce}", std::process::id())
        } else {
            std::env::temp_dir()
                .join(format!(
                    "fbuild-platform-ipc-{}-{nonce}.sock",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned()
        }
    }

    #[test]
    fn local_endpoint_round_trip_and_peer_facts() {
        let endpoint = unique_endpoint();
        let listener = LocalListener::bind(&endpoint).expect("bind local endpoint");
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().expect("accept local client");
            let facts = stream.peer_facts().expect("query peer facts");
            if !crate::platform::host::is_macos() {
                assert_eq!(facts.pid, Some(std::process::id()));
            }
            if crate::platform::host::is_unix() {
                assert!(facts.user_id.is_some());
            }
            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).expect("read request");
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").expect("write response");
        });

        let mut client = connect(&endpoint).expect("connect local endpoint");
        client.write_all(b"ping").expect("write request");
        let mut response = [0_u8; 4];
        client.read_exact(&mut response).expect("read response");
        assert_eq!(&response, b"pong");
        server.join().expect("server thread");
    }

    #[tokio::test]
    async fn tcp_readiness_distinguishes_live_and_free_endpoints() {
        let listener =
            bind_tcp_listener("127.0.0.1:0".parse().unwrap()).expect("bind ephemeral listener");
        let address = listener.local_addr().expect("listener address");
        assert!(tcp_endpoint_ready(address, Duration::from_millis(500)).await);
        drop(listener);
        assert!(!tcp_endpoint_ready(address, Duration::from_millis(100)).await);
    }
}
