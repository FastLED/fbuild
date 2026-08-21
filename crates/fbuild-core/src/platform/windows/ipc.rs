use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions};
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use socket2::{Domain, Protocol, Socket, Type};
use std::os::windows::io::AsRawSocket;

pub(crate) type LocalListener = LocalSocketListener;
pub(crate) type LocalStream = LocalSocketStream;

pub(crate) fn bind_local(endpoint: &str) -> std::io::Result<LocalListener> {
    let name = endpoint.to_ns_name::<GenericNamespaced>()?;
    let security = SecurityDescriptor::deserialize(widestring::u16cstr!(
        "D:P(A;;GA;;;OW)"
    ))?;
    ListenerOptions::new()
        .name(name)
        .security_descriptor(security)
        .create_sync()
}

pub(crate) fn connect_local(endpoint: &str) -> std::io::Result<LocalStream> {
    let name = endpoint.to_ns_name::<GenericNamespaced>()?;
    LocalSocketStream::connect(name)
}

pub(crate) fn accept(listener: &LocalListener) -> std::io::Result<LocalStream> {
    listener.accept()
}

pub(crate) fn peer_facts(stream: &LocalStream) -> std::io::Result<super::super::ipc::PeerFacts> {
    let credentials = stream.peer_creds()?;
    Ok(super::super::ipc::PeerFacts {
        pid: credentials.pid(),
        user_id: None,
        group_id: None,
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
    if let Err(error) = set_exclusive_address(&socket) {
        tracing::warn!("failed to set SO_EXCLUSIVEADDRUSE: {error}");
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

fn set_exclusive_address(socket: &Socket) -> std::io::Result<()> {
    const SOL_SOCKET: i32 = 0xFFFF;
    const SO_EXCLUSIVEADDRUSE: i32 = !0x0004;

    type SocketHandle = usize;
    #[link(name = "ws2_32")]
    extern "system" {
        fn setsockopt(
            socket: SocketHandle,
            level: i32,
            option_name: i32,
            option_value: *const u8,
            option_length: i32,
        ) -> i32;
    }

    let enabled = 1_i32;
    // SAFETY: the socket is live and `enabled` remains readable for the exact
    // byte length supplied to Winsock.
    let result = unsafe {
        setsockopt(
            socket.as_raw_socket() as SocketHandle,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&enabled as *const i32).cast(),
            std::mem::size_of::<i32>() as i32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    type Handle = *mut std::ffi::c_void;

    #[test]
    fn anonymous_identity_cannot_connect_to_owner_only_endpoint() {
        let endpoint = format!(
            "fbuild-platform-ipc-owner-only-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let _listener = super::bind_local(&endpoint).unwrap();
        let result = std::thread::spawn(move || {
            // SAFETY: GetCurrentThread returns the calling thread's valid
            // pseudo-handle, accepted by ImpersonateAnonymousToken.
            let impersonated = unsafe { ImpersonateAnonymousToken(GetCurrentThread()) };
            assert_ne!(impersonated, 0, "anonymous impersonation failed");
            let connect = super::connect_local(&endpoint).map(|_| ());
            // SAFETY: this thread successfully entered impersonation above and
            // reverts itself before it exits.
            let reverted = unsafe { RevertToSelf() };
            assert_ne!(reverted, 0, "failed to revert anonymous impersonation");
            connect
        })
        .join()
        .unwrap();

        let error = result.expect_err("anonymous identity must not open owner-only endpoint");
        assert_eq!(error.raw_os_error(), Some(5));
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> Handle;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ImpersonateAnonymousToken(thread_handle: Handle) -> i32;
        fn RevertToSelf() -> i32;
    }
}
