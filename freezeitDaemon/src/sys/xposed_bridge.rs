use std::{
    io::{self, Read, Write},
    mem,
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use crate::{
    app::error::DaemonError,
    protocol::xposed::{
        encode_frame, parse_foreground_uid_payload, XposedCommand, MAX_PAYLOAD_LEN,
    },
};

pub const XPOSED_SOCKET_ABSTRACT_NAME: &str = "FreezeitXposedServer";
const XPOSED_RESPONSE_DEADLINE: Duration = Duration::from_secs(3);
const XPOSED_RESPONSE_MAX_BYTES: usize = MAX_PAYLOAD_LEN;
const ROOT_UID: libc::uid_t = 0;
const SYSTEM_SERVER_UID: libc::uid_t = 1_000;

pub fn query_hook_health() -> Result<String, DaemonError> {
    request_text(XposedCommand::GetHookHealth, &[])
}

pub fn query_foreground_uids() -> Result<Vec<u32>, DaemonError> {
    let response = request_bytes(XposedCommand::GetForeground, &[])?;
    parse_foreground_uid_payload(&response)
}

pub fn set_config(payload: &[u8]) -> Result<bool, DaemonError> {
    let response = request_bytes(XposedCommand::SetConfig, payload)?;
    if response.len() < 4 {
        return Err(DaemonError::protocol(
            "xposed set config response header is incomplete",
        ));
    }

    Ok(i32::from_le_bytes([response[0], response[1], response[2], response[3]]) == 2)
}

pub fn request_text(command: XposedCommand, payload: &[u8]) -> Result<String, DaemonError> {
    let response = request_bytes(command, payload)?;
    String::from_utf8(response)
        .map_err(|error| DaemonError::protocol(format!("xposed response is not utf-8: {error}")))
}

pub fn request_bytes(command: XposedCommand, payload: &[u8]) -> Result<Vec<u8>, DaemonError> {
    let request = encode_frame(command, payload)?;
    let mut stream = connect_abstract_socket(XPOSED_SOCKET_ABSTRACT_NAME)?;
    stream
        .set_read_timeout(Some(XPOSED_RESPONSE_DEADLINE))
        .map_err(DaemonError::from)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(DaemonError::from)?;
    stream.write_all(&request).map_err(DaemonError::from)?;

    read_response_with_deadline(&mut stream, Instant::now() + XPOSED_RESPONSE_DEADLINE)
        .map_err(DaemonError::from)
}

fn connect_abstract_socket(name: &str) -> Result<UnixStream, DaemonError> {
    let name_bytes = name.as_bytes();
    let max_name_len =
        mem::size_of::<libc::sockaddr_un>() - mem::size_of::<libc::sa_family_t>() - 1;
    if name_bytes.len() > max_name_len {
        return Err(DaemonError::system("xposed socket name is too long"));
    }

    // SAFETY: socket/connect are called with a fully initialized sockaddr_un and
    // the returned fd is transferred into UnixStream exactly once on success.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(DaemonError::from(std::io::Error::last_os_error()));
        }

        let mut addr: libc::sockaddr_un = mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        addr.sun_path[0] = 0;
        for (index, byte) in name_bytes.iter().enumerate() {
            addr.sun_path[index + 1] = *byte as libc::c_char;
        }

        let len = (mem::size_of::<libc::sa_family_t>() + 1 + name_bytes.len()) as libc::socklen_t;
        let result = libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len);
        if result < 0 {
            let error = std::io::Error::last_os_error();
            libc::close(fd);
            return Err(DaemonError::from(error));
        }

        let stream = UnixStream::from_raw_fd(fd);
        authenticate_hook_peer(stream.as_raw_fd())?;
        Ok(stream)
    }
}

fn authenticate_hook_peer(fd: RawFd) -> Result<(), DaemonError> {
    let mut credentials: libc::ucred = unsafe { mem::zeroed() };
    let mut credentials_len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut credentials_len,
        )
    };
    if result < 0 {
        return Err(DaemonError::from(io::Error::last_os_error()));
    }
    if credentials_len != mem::size_of::<libc::ucred>() as libc::socklen_t {
        return Err(DaemonError::system(
            "xposed peer credential response is incomplete",
        ));
    }
    if !is_authorized_hook_peer_uid(credentials.uid) {
        return Err(DaemonError::system(format!(
            "refusing xposed bridge peer with uid {}",
            credentials.uid
        )));
    }
    Ok(())
}

fn is_authorized_hook_peer_uid(uid: libc::uid_t) -> bool {
    // The bridge server is injected into system_server. Root is permitted for controlled
    // recovery/testing, but another app that claims the global abstract name is not trusted.
    matches!(uid, ROOT_UID | SYSTEM_SERVER_UID)
}

fn read_response_with_deadline(stream: &mut UnixStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];

    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "xposed response deadline exceeded")
            })?;
        stream.set_read_timeout(Some(remaining))?;

        match stream.read(&mut chunk) {
            Ok(0) => return Ok(response),
            Ok(read) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "xposed response deadline exceeded",
                    ));
                }
                append_response_chunk(&mut response, &chunk[..read])?;
            }
            Err(error) => {
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) && Instant::now() >= deadline
                {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "xposed response deadline exceeded",
                    ));
                }
                return Err(error);
            }
        }
    }
}

fn append_response_chunk(response: &mut Vec<u8>, chunk: &[u8]) -> io::Result<()> {
    let new_len = response.len().checked_add(chunk.len()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "xposed response is too large")
    })?;
    if new_len > XPOSED_RESPONSE_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "xposed response is too large",
        ));
    }
    response.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_root_or_system_server_can_own_the_hook_bridge() {
        assert!(is_authorized_hook_peer_uid(ROOT_UID));
        assert!(is_authorized_hook_peer_uid(SYSTEM_SERVER_UID));
        assert!(!is_authorized_hook_peer_uid(10_123));
    }

    #[test]
    fn response_reader_rejects_oversized_payloads() {
        let mut response = Vec::new();
        let error =
            append_response_chunk(&mut response, &vec![0_u8; XPOSED_RESPONSE_MAX_BYTES + 1])
                .expect_err("oversized response must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
