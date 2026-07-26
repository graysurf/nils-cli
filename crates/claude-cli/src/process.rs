use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
#[cfg(not(unix))]
use std::sync::Arc;
#[cfg(not(unix))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(not(unix))]
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
#[cfg(not(unix))]
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const SPAWN_RETRY_BUDGET: Duration = Duration::from_secs(2);

#[cfg(not(unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub(crate) enum ProcessOutputError {
    Io(io::Error),
    Timeout,
    OutputLimit,
}

#[derive(Debug)]
pub(crate) enum ProcessStatusError {
    Launch,
    Wait,
    Timeout,
    Failed,
}

pub(crate) fn status_with_deadline(
    command: &mut Command,
    timeout: Duration,
) -> Result<ExitStatus, ProcessStatusError> {
    configure_process_group(command);
    let mut child = spawn_with_retry(command).map_err(|_| ProcessStatusError::Launch)?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(_) => {
                terminate_process_group(&mut child);
                return Err(ProcessStatusError::Wait);
            }
        }
        if started.elapsed() >= timeout {
            terminate_process_group(&mut child);
            return Err(ProcessStatusError::Timeout);
        }
        thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
    }
}

pub(crate) fn output_with_limits(
    command: &mut Command,
    timeout: Duration,
    capture_limit: usize,
) -> Result<Output, ProcessOutputError> {
    #[cfg(unix)]
    {
        output_with_limits_unix(command, timeout, capture_limit)
    }
    #[cfg(not(unix))]
    {
        output_with_limits_threaded(command, timeout, capture_limit)
    }
}

#[cfg(unix)]
fn output_with_limits_unix(
    command: &mut Command,
    timeout: Duration,
    capture_limit: usize,
) -> Result<Output, ProcessOutputError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_process_group(command);
    let mut child = spawn_with_retry(command).map_err(ProcessOutputError::Io)?;
    let mut stdout_reader = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            return Err(ProcessOutputError::Io(io::Error::other(
                "child stdout was not piped",
            )));
        }
    };
    let mut stderr_reader = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            return Err(ProcessOutputError::Io(io::Error::other(
                "child stderr was not piped",
            )));
        }
    };
    if let Err(error) =
        set_nonblocking(&stdout_reader).and_then(|()| set_nonblocking(&stderr_reader))
    {
        terminate_process_group(&mut child);
        return Err(ProcessOutputError::Io(error));
    }

    let started = Instant::now();
    let mut status = None;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut captured_total = 0usize;
    let mut stdout_closed = false;
    let mut stderr_closed = false;

    loop {
        if !stdout_closed {
            match drain_nonblocking(
                &mut stdout_reader,
                &mut stdout,
                &mut captured_total,
                capture_limit,
            ) {
                Ok(closed) => stdout_closed = closed,
                Err(error) => {
                    terminate_process_group(&mut child);
                    return Err(error);
                }
            }
        }
        if !stderr_closed {
            match drain_nonblocking(
                &mut stderr_reader,
                &mut stderr,
                &mut captured_total,
                capture_limit,
            ) {
                Ok(closed) => stderr_closed = closed,
                Err(error) => {
                    terminate_process_group(&mut child);
                    return Err(error);
                }
            }
        }

        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    terminate_process_group(&mut child);
                    return Err(ProcessOutputError::Io(error));
                }
            };
        }
        if let Some(status) = status
            && stdout_closed
            && stderr_closed
        {
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if started.elapsed() >= timeout {
            terminate_process_group(&mut child);
            return Err(ProcessOutputError::Timeout);
        }
        thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
    }
}

#[cfg(unix)]
fn set_nonblocking(reader: &impl AsRawFd) -> io::Result<()> {
    let descriptor = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn drain_nonblocking(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
    captured_total: &mut usize,
    capture_limit: usize,
) -> Result<bool, ProcessOutputError> {
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                let reserved = read.min(capture_limit.saturating_sub(*captured_total));
                output.extend_from_slice(&chunk[..reserved]);
                *captured_total += reserved;
                if reserved < read {
                    return Err(ProcessOutputError::OutputLimit);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ProcessOutputError::Io(error)),
        }
    }
}

#[cfg(not(unix))]
fn output_with_limits_threaded(
    command: &mut Command,
    timeout: Duration,
    capture_limit: usize,
) -> Result<Output, ProcessOutputError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_process_group(command);
    let mut child = spawn_with_retry(command).map_err(ProcessOutputError::Io)?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            return Err(ProcessOutputError::Io(io::Error::other(
                "child stdout was not piped",
            )));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            return Err(ProcessOutputError::Io(io::Error::other(
                "child stderr was not piped",
            )));
        }
    };

    let (sender, receiver) = mpsc::channel();
    let captured_total = Arc::new(AtomicUsize::new(0));
    let stdout_reader = spawn_reader(
        stdout,
        OutputStream::Stdout,
        capture_limit,
        Arc::clone(&captured_total),
        sender.clone(),
    );
    let stderr_reader = spawn_reader(
        stderr,
        OutputStream::Stderr,
        capture_limit,
        captured_total,
        sender,
    );
    let mut readers = Some([stdout_reader, stderr_reader]);
    let started = Instant::now();
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;

    loop {
        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    terminate_process_group(&mut child);
                    join_readers(&mut readers);
                    return Err(ProcessOutputError::Io(error));
                }
            };
        }
        if let Some(exit_status) = status
            && stdout.is_some()
            && stderr.is_some()
        {
            let Some(stdout) = stdout.take() else {
                unreachable!("stdout was checked");
            };
            let Some(stderr) = stderr.take() else {
                unreachable!("stderr was checked");
            };
            join_readers(&mut readers);
            return Ok(Output {
                status: exit_status,
                stdout,
                stderr,
            });
        }

        if started.elapsed() >= timeout {
            terminate_process_group(&mut child);
            join_readers(&mut readers);
            return Err(ProcessOutputError::Timeout);
        }

        let wait = POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed()));
        match receiver.recv_timeout(wait) {
            Ok(ReaderEvent {
                stream,
                result: Ok(captured),
            }) => {
                if captured.limit_exceeded {
                    terminate_process_group(&mut child);
                    join_readers(&mut readers);
                    return Err(ProcessOutputError::OutputLimit);
                }
                match stream {
                    OutputStream::Stdout => stdout = Some(captured.output),
                    OutputStream::Stderr => stderr = Some(captured.output),
                }
            }
            Ok(ReaderEvent {
                result: Err(error), ..
            }) => {
                terminate_process_group(&mut child);
                join_readers(&mut readers);
                return Err(ProcessOutputError::Io(error));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                terminate_process_group(&mut child);
                join_readers(&mut readers);
                return Err(ProcessOutputError::Io(io::Error::other(
                    "child output readers disconnected",
                )));
            }
        }
    }
}

pub(crate) fn output_with_limits_retry_io(
    command: &mut Command,
    timeout: Duration,
    capture_limit: usize,
    attempts: usize,
) -> Result<Output, ProcessOutputError> {
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        match output_with_limits(command, timeout, capture_limit) {
            Err(ProcessOutputError::Io(_)) if attempt + 1 < attempts => {
                thread::sleep(POLL_INTERVAL);
            }
            result => return result,
        }
    }
    unreachable!("at least one process attempt is always made")
}

fn spawn_with_retry(command: &mut Command) -> io::Result<Child> {
    retry_would_block(|| command.spawn(), SPAWN_RETRY_BUDGET)
}

fn retry_would_block<T>(
    mut operation: impl FnMut() -> io::Result<T>,
    retry_budget: Duration,
) -> io::Result<T> {
    let started = Instant::now();
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_transient_spawn_error(&error) && started.elapsed() < retry_budget => {
                thread::sleep(POLL_INTERVAL.min(retry_budget.saturating_sub(started.elapsed())));
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_transient_spawn_error(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error().is_some_and(|code| {
        [libc::EAGAIN, libc::ETXTBSY, libc::EMFILE, libc::ENFILE].contains(&code)
    }) {
        return true;
    }
    false
}

#[cfg(not(unix))]
fn spawn_reader<R>(
    mut reader: R,
    stream: OutputStream,
    capture_limit: usize,
    captured_total: Arc<AtomicUsize>,
    sender: mpsc::Sender<ReaderEvent>,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 8192];
        let result = loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    break Ok(CapturedStream {
                        output,
                        limit_exceeded: false,
                    });
                }
                Ok(read) => {
                    let reserved = reserve_capture_bytes(&captured_total, read, capture_limit);
                    output.extend_from_slice(&chunk[..reserved]);
                    if reserved < read {
                        break Ok(CapturedStream {
                            output,
                            limit_exceeded: true,
                        });
                    }
                }
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(ReaderEvent { stream, result });
    })
}

#[cfg(not(unix))]
fn reserve_capture_bytes(total: &AtomicUsize, requested: usize, limit: usize) -> usize {
    let mut current = total.load(Ordering::Acquire);
    loop {
        let reserved = requested.min(limit.saturating_sub(current));
        match total.compare_exchange_weak(
            current,
            current + reserved,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return reserved,
            Err(observed) => current = observed,
        }
    }
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}

fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let process_group = -(child.id() as libc::pid_t);
        let _ = libc::kill(process_group, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn join_readers(readers: &mut Option<[JoinHandle<()>; 2]>) {
    if let Some(readers) = readers.take() {
        for reader in readers {
            let _ = reader.join();
        }
    }
}

#[cfg(not(unix))]
struct ReaderEvent {
    stream: OutputStream,
    result: io::Result<CapturedStream>,
}

#[cfg(not(unix))]
struct CapturedStream {
    output: Vec<u8>,
    limit_exceeded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn captures_bounded_stdout_stderr_and_status() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf stdout; printf stderr >&2; exit 7"])
            .stdin(Stdio::null());

        let output =
            output_with_limits(&mut command, Duration::from_secs(1), 1024).expect("output");

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }

    #[test]
    fn terminates_a_hung_process_at_the_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]).stdin(Stdio::null());
        let started = Instant::now();

        let error = output_with_limits(&mut command, Duration::from_millis(100), 1024)
            .expect_err("timeout");

        assert!(matches!(error, ProcessOutputError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn terminates_a_process_while_output_is_still_growing() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while :; do printf 1234567890; done"])
            .stdin(Stdio::null());
        let started = Instant::now();

        let error = output_with_limits(&mut command, Duration::from_secs(2), 1024)
            .expect_err("output limit");

        assert!(matches!(error, ProcessOutputError::OutputLimit));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn enforces_one_aggregate_limit_across_stdout_and_stderr() {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf 12345678; printf abcdefgh >&2; printf overflow",
            ])
            .stdin(Stdio::null());

        let error = output_with_limits(&mut command, Duration::from_secs(1), 12)
            .expect_err("aggregate output limit");

        assert!(matches!(error, ProcessOutputError::OutputLimit));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_deadline_is_bounded_when_a_descendant_escapes_with_the_pipes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_file = tmp.path().join("escaped.pid");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                &format!(
                    "setsid sh -c 'sleep 30' & printf '%s' \"$!\" > '{}'",
                    pid_file.display()
                ),
            ])
            .stdin(Stdio::null());
        let started = Instant::now();

        let error = output_with_limits(&mut command, Duration::from_millis(100), 1024)
            .expect_err("escaped descendant must not hold the caller");

        assert!(matches!(error, ProcessOutputError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(pid_file)
            .expect("escaped pid")
            .parse::<libc::pid_t>()
            .expect("numeric escaped pid");
        unsafe {
            let _ = libc::kill(-pid, libc::SIGKILL);
            let _ = libc::kill(pid, libc::SIGKILL);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_limit_is_bounded_when_a_descendant_escapes_with_the_pipes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_file = tmp.path().join("escaped.pid");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                &format!(
                    "setsid sh -c 'sleep 30' & printf '%s' \"$!\" > '{}'; printf overflow",
                    pid_file.display()
                ),
            ])
            .stdin(Stdio::null());
        let started = Instant::now();

        let error = output_with_limits(&mut command, Duration::from_secs(5), 4)
            .expect_err("escaped descendant must not hold the caller");

        assert!(matches!(error, ProcessOutputError::OutputLimit));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(pid_file)
            .expect("escaped pid")
            .parse::<libc::pid_t>()
            .expect("numeric escaped pid");
        unsafe {
            let _ = libc::kill(-pid, libc::SIGKILL);
            let _ = libc::kill(pid, libc::SIGKILL);
        }
    }

    #[test]
    fn status_deadline_terminates_a_hung_process_group() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sentinel = tmp.path().join("descendant-survived");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                &format!(
                    "(sleep 0.4; printf survived > '{}') & wait",
                    sentinel.display()
                ),
            ])
            .stdin(Stdio::null());
        let started = Instant::now();

        let error = status_with_deadline(&mut command, Duration::from_millis(100))
            .expect_err("status timeout");

        assert!(matches!(error, ProcessStatusError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(500));
        assert!(
            !sentinel.exists(),
            "a descendant survived the process-group timeout"
        );
    }

    #[test]
    fn spawn_retry_is_bounded_and_limited_to_would_block() {
        let mut attempts = 0;
        let value = retry_would_block(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                } else {
                    Ok(7)
                }
            },
            Duration::from_millis(100),
        )
        .expect("transient spawn recovery");
        assert_eq!(value, 7);
        assert_eq!(attempts, 3);

        #[cfg(unix)]
        {
            let mut descriptor_attempts = 0;
            let value = retry_would_block(
                || {
                    descriptor_attempts += 1;
                    if descriptor_attempts < 2 {
                        Err(io::Error::from_raw_os_error(libc::EMFILE))
                    } else {
                        Ok(9)
                    }
                },
                Duration::from_millis(100),
            )
            .expect("transient descriptor exhaustion recovery");
            assert_eq!(value, 9);
            assert_eq!(descriptor_attempts, 2);
        }

        let mut permanent_attempts = 0;
        let error = retry_would_block(
            || {
                permanent_attempts += 1;
                Err::<(), _>(io::Error::from(io::ErrorKind::NotFound))
            },
            Duration::from_millis(100),
        )
        .expect_err("permanent spawn failure");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(permanent_attempts, 1);
    }
}
