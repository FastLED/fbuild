use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
use socket2::{Domain, Protocol, Socket, Type};

pub(crate) type LocalListener = LocalSocketListener;
pub(crate) type LocalStream = LocalSocketStream;

pub(crate) fn bind_local(endpoint: &str) -> std::io::Result<LocalListener> {
    if let Some(parent) = std::path::Path::new(endpoint).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(endpoint);
    let name = endpoint.to_fs_name::<GenericFilePath>()?;
    // Darwin does not support fchmod() on socket fds; interprocess maps the
    // EINVAL to ErrorKind::Unsupported when ListenerOptions::mode() is used.
    // Enforce owner-only by chmodding the bound socket path instead. Linux
    // keeps the atomic pre-bind fchmod via mode().
    let listener = ListenerOptions::new().name(name).create_sync()?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub(crate) fn connect_local(endpoint: &str) -> std::io::Result<LocalStream> {
    let name = endpoint.to_fs_name::<GenericFilePath>()?;
    LocalSocketStream::connect(name)
}

pub(crate) fn accept(listener: &LocalListener) -> std::io::Result<LocalStream> {
    listener.accept()
}

pub(crate) fn peer_facts(stream: &LocalStream) -> std::io::Result<super::super::ipc::PeerFacts> {
    let credentials = stream.peer_creds()?;
    Ok(super::super::ipc::PeerFacts {
        pid: credentials.pid().and_then(|pid| u32::try_from(pid).ok()),
        user_id: credentials.euid(),
        group_id: credentials.egid(),
    })
}

pub(crate) fn bind_tcp_listener(
    address: std::net::SocketAddr,
) -> Result<tokio::net::TcpListener, super::super::ipc::TcpListenerError> {
    use super::super::ipc::TcpListenerError;
    let domain = if address.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(|source| TcpListenerError::setup("create TCP socket", source))?;
    if let Err(error) = socket.set_reuse_address(true) {
        tracing::warn!("failed to set SO_REUSEADDR: {error}");
    }
    if let Err(error) = socket.set_linger(Some(std::time::Duration::ZERO)) {
        tracing::warn!("failed to set SO_LINGER=0 on listener: {error}");
    }
    socket
        .set_nonblocking(true)
        .map_err(|source| TcpListenerError::setup("set TCP listener nonblocking", source))?;
    socket.bind(&address.into()).map_err(TcpListenerError::Bind)?;
    socket
        .listen(128)
        .map_err(|source| TcpListenerError::setup("listen on TCP endpoint", source))?;
    tokio::net::TcpListener::from_std(socket.into())
        .map_err(|source| TcpListenerError::setup("convert TCP listener to Tokio", source))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn local_endpoint_is_owner_only() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = directory.path().join("owner-only.sock");
        let endpoint_text = endpoint.to_string_lossy();
        let listener = super::bind_local(&endpoint_text).unwrap();
        let mode = std::fs::metadata(&endpoint).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
        drop(listener);
    }
}
