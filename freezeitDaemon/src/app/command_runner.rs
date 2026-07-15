use std::{
    io::{self, Read},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use crate::app::error::DaemonError;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const READ_BUFFER_SIZE: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn run_command(program: &str, args: &[&str]) -> io::Result<CommandOutput> {
    run_command_with_limits(program, args, COMMAND_TIMEOUT, MAX_OUTPUT_BYTES)
        .map_err(daemon_error_to_io)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamReadState {
    Open,
    Closed,
    LimitExceeded,
}

fn run_command_with_limits(
    program: &str,
    args: &[&str],
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<CommandOutput, DaemonError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .expect("piped stdout must be available after spawning command");
    let mut stderr = child
        .stderr
        .take()
        .expect("piped stderr must be available after spawning command");

    if let Err(error) = set_nonblocking(&stdout).and_then(|_| set_nonblocking(&stderr)) {
        stop_child(&mut child);
        return Err(error.into());
    }

    let deadline = Instant::now() + timeout;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;

    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(child_status) => status = child_status,
                Err(error) => {
                    stop_child(&mut child);
                    return Err(error.into());
                }
            }
        }

        if status.is_some() && !stdout_open && !stderr_open {
            return Ok(CommandOutput {
                status: status.expect("child status is checked above"),
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            });
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_child(&mut child);
            return Err(DaemonError::system(format!(
                "command `{program}` timed out after {} ms",
                timeout.as_millis()
            )));
        }

        let mut descriptors = [
            poll_descriptor(&stdout, stdout_open),
            poll_descriptor(&stderr, stderr_open),
        ];
        let poll_timeout = remaining.min(POLL_INTERVAL).as_millis().max(1) as i32;
        let poll_result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                poll_timeout,
            )
        };
        if poll_result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            stop_child(&mut child);
            return Err(error.into());
        }
        if poll_result == 0 {
            continue;
        }

        if descriptors[0].revents != 0 {
            let stdout_limit = max_output_bytes.saturating_sub(stderr_bytes.len());
            let stream_state = match read_stream_chunk(&mut stdout, &mut stdout_bytes, stdout_limit)
            {
                Ok(stream_state) => stream_state,
                Err(error) => {
                    stop_child(&mut child);
                    return Err(error.into());
                }
            };
            match stream_state {
                StreamReadState::Open => {}
                StreamReadState::Closed => stdout_open = false,
                StreamReadState::LimitExceeded => {
                    stop_child(&mut child);
                    return Err(output_limit_error(program, max_output_bytes));
                }
            }
        }
        if descriptors[1].revents != 0 {
            let stderr_limit = max_output_bytes.saturating_sub(stdout_bytes.len());
            let stream_state = match read_stream_chunk(&mut stderr, &mut stderr_bytes, stderr_limit)
            {
                Ok(stream_state) => stream_state,
                Err(error) => {
                    stop_child(&mut child);
                    return Err(error.into());
                }
            };
            match stream_state {
                StreamReadState::Open => {}
                StreamReadState::Closed => stderr_open = false,
                StreamReadState::LimitExceeded => {
                    stop_child(&mut child);
                    return Err(output_limit_error(program, max_output_bytes));
                }
            }
        }
    }
}

fn poll_descriptor(stream: &impl AsRawFd, open: bool) -> libc::pollfd {
    libc::pollfd {
        fd: open.then(|| stream.as_raw_fd()).unwrap_or(-1),
        events: libc::POLLIN,
        revents: 0,
    }
}

fn set_nonblocking(stream: &impl AsRawFd) -> io::Result<()> {
    let file_descriptor = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(file_descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(file_descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_stream_chunk(
    stream: &mut impl Read,
    output: &mut Vec<u8>,
    max_output_bytes: usize,
) -> io::Result<StreamReadState> {
    let mut buffer = [0; READ_BUFFER_SIZE];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(StreamReadState::Closed),
            Ok(bytes_read) => {
                if bytes_read > max_output_bytes.saturating_sub(output.len()) {
                    return Ok(StreamReadState::LimitExceeded);
                }
                output.extend_from_slice(&buffer[..bytes_read]);
                return Ok(StreamReadState::Open);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(StreamReadState::Open);
            }
            Err(error) => return Err(error),
        }
    }
}

fn output_limit_error(program: &str, max_output_bytes: usize) -> DaemonError {
    DaemonError::system(format!(
        "command `{program}` exceeded combined output limit of {max_output_bytes} bytes"
    ))
}

fn stop_child(child: &mut Child) {
    let process_group_id = child.id() as libc::pid_t;
    if unsafe { libc::kill(-process_group_id, libc::SIGKILL) } == -1 {
        let _ = child.kill();
    }
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.wait();
    }
}

fn daemon_error_to_io(error: DaemonError) -> io::Error {
    match error {
        DaemonError::Io(error) => error,
        error => io::Error::new(io::ErrorKind::Other, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::error::DaemonError;
    use std::time::Duration;

    #[test]
    fn returns_system_error_when_child_exceeds_timeout() {
        let started_at = Instant::now();
        let error = run_command_with_limits(
            "sh",
            &["-c", "exec sleep 2"],
            Duration::from_millis(50),
            1024,
        )
        .expect_err("long-running child must time out");

        assert!(
            started_at.elapsed() < Duration::from_millis(500),
            "timed-out child must be terminated rather than waited out"
        );

        assert!(matches!(
            error,
            DaemonError::System(message) if message.contains("timed out")
        ));
    }

    #[test]
    fn returns_system_error_when_child_exceeds_output_limit() {
        let started_at = Instant::now();
        let error = run_command_with_limits(
            "sh",
            &["-c", "printf 123456789; exec sleep 2"],
            Duration::from_secs(1),
            8,
        )
        .expect_err("oversized child output must be rejected");

        assert!(
            started_at.elapsed() < Duration::from_millis(500),
            "output-limited child must be terminated rather than waited out"
        );

        assert!(matches!(
            error,
            DaemonError::System(message) if message.contains("output limit")
        ));
    }

    #[test]
    fn returns_system_error_when_combined_stream_output_exceeds_limit() {
        let started_at = Instant::now();
        let error = run_command_with_limits(
            "sh",
            &["-c", "printf 12345678; printf 9 >&2; exec sleep 2"],
            Duration::from_secs(1),
            8,
        )
        .expect_err("combined stdout and stderr output must be rejected");

        assert!(
            started_at.elapsed() < Duration::from_millis(500),
            "output-limited child must be terminated rather than waited out"
        );
        assert!(matches!(
            error,
            DaemonError::System(message) if message.contains("output limit")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn output_limit_kills_background_descendants_in_command_process_group() {
        use std::{
            fs, process, thread,
            time::{SystemTime, UNIX_EPOCH},
        };

        let token = format!(
            "freezeit-command-runner-{}-{}",
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time is after Unix epoch")
                .as_nanos()
        );
        let temporary_directory = std::env::temp_dir();
        let ready_marker = temporary_directory.join(format!("{token}.ready"));
        let delayed_marker = temporary_directory.join(format!("{token}.marker"));
        let ready_marker_path = ready_marker.to_string_lossy().into_owned();
        let delayed_marker_path = delayed_marker.to_string_lossy().into_owned();

        let error = run_command_with_limits(
            "sh",
            &[
                "-c",
                "(touch \"$1\"; sleep 0.2; touch \"$2\") & while [ ! -e \"$1\" ]; do :; done; printf x; wait",
                "freezeit-command-runner",
                &ready_marker_path,
                &delayed_marker_path,
            ],
            Duration::from_secs(1),
            0,
        )
        .expect_err("command must exceed the output limit after its background child starts");

        assert!(matches!(
            error,
            DaemonError::System(message) if message.contains("output limit")
        ));
        assert!(
            ready_marker.exists(),
            "output must be emitted only after the background child starts"
        );

        thread::sleep(Duration::from_millis(300));
        let delayed_marker_exists = delayed_marker.exists();
        let _ = fs::remove_file(ready_marker);
        let _ = fs::remove_file(delayed_marker);
        assert!(
            !delayed_marker_exists,
            "output-limit cleanup must terminate background descendants as well as the direct child"
        );
    }
}
