use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub elapsed_ms: u64,
}

pub fn run(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    removed_envs: &[&str],
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<ProcessOutput, String> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    #[cfg(unix)]
    // SAFETY: `setpgid(0, 0)` performs only the async-signal-safe syscall and
    // gives the child a private process group so timeout cleanup includes any
    // helper process that inherited its captured stdio descriptors.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start process: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture process stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture process stderr".to_string())?;
    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));

    let stdin_writer = if let Some(bytes) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open process stdin".to_string())?;
        let bytes = bytes.to_vec();
        Some(thread::spawn(move || stdin.write_all(&bytes)))
    } else {
        None
    };
    drop(child.stdin.take());

    let mut timed_out = false;
    let status = loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to wait for process: {error}"))?
        {
            Some(status) => break status,
            None if started.elapsed() >= timeout => {
                timed_out = true;
                #[cfg(unix)]
                {
                    let group = -(child.id() as i32);
                    if unsafe { libc::kill(group, libc::SIGKILL) } != 0 {
                        child.kill().map_err(|error| {
                            format!("failed to stop timed-out process: {error}")
                        })?;
                    }
                }
                #[cfg(not(unix))]
                child
                    .kill()
                    .map_err(|error| format!("failed to stop timed-out process: {error}"))?;
                break child
                    .wait()
                    .map_err(|error| format!("failed to reap timed-out process: {error}"))?;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };

    #[cfg(unix)]
    if !timed_out {
        // The direct child may have exited after leaving helpers that inherited
        // captured pipes. Close the whole execution group before joining the
        // readers so pipe draining remains bounded by the caller's deadline.
        let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    }

    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "stdout reader thread failed".to_string())?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "stderr reader thread failed".to_string())?;
    if let Some(writer) = stdin_writer {
        match writer
            .join()
            .map_err(|_| "stdin writer thread failed".to_string())?
        {
            Ok(()) => {}
            Err(error) if timed_out || error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(format!("failed to write process stdin: {error}")),
        }
    }

    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;

    Ok(ProcessOutput {
        exit_code: status.code().unwrap_or(if timed_out { 124 } else { 1 }),
        signal,
        stdout,
        stderr,
        timed_out,
        stdout_truncated,
        stderr_truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

pub fn run_detached(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    removed_envs: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput, String> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    for key in removed_envs {
        command.env_remove(key);
    }
    #[cfg(unix)]
    // SAFETY: `setpgid(0, 0)` is async-signal-safe and isolates the launcher
    // while allowing a successfully detached daemon to outlive it.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start detached process: {error}"))?;
    let mut timed_out = false;
    let status = loop {
        match child
            .try_wait()
            .map_err(|error| format!("failed to wait for detached process: {error}"))?
        {
            Some(status) => break status,
            None if started.elapsed() >= timeout => {
                timed_out = true;
                #[cfg(unix)]
                {
                    let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                }
                #[cfg(not(unix))]
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("failed to reap detached launcher: {error}"))?;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    Ok(ProcessOutput {
        exit_code: status.code().unwrap_or(if timed_out { 124 } else { 1 }),
        signal,
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out,
        stdout_truncated: false,
        stderr_truncated: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn read_capped(mut reader: impl Read) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(kept.len());
        let retain = remaining.min(read);
        kept.extend_from_slice(&buffer[..retain]);
        truncated |= retain < read;
    }
    (kept, truncated)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::run;

    #[test]
    fn captures_stdout_stderr_and_exit() {
        let output = run(
            Path::new("/bin/sh"),
            &["-c".into(), "printf out; printf err >&2; exit 7".into()],
            &[],
            &[],
            None,
            Duration::from_secs(1),
        )
        .expect("process");
        assert_eq!(output.exit_code, 7);
        assert_eq!(output.signal, None);
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        assert!(!output.timed_out);
    }

    #[test]
    fn kills_a_timed_out_process() {
        let started = std::time::Instant::now();
        let output = run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 2".into()],
            &[],
            &[],
            None,
            Duration::from_millis(20),
        )
        .expect("process");
        assert!(output.timed_out);
        assert_eq!(output.exit_code, 124);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn stdin_delivery_is_covered_by_the_process_deadline() {
        let input = vec![b'x'; 1024 * 1024];
        let started = std::time::Instant::now();
        let output = run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 2".into()],
            &[],
            &[],
            Some(&input),
            Duration::from_millis(30),
        )
        .expect("process");
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn direct_child_exit_cleans_helpers_that_hold_capture_pipes() {
        let started = std::time::Instant::now();
        let output = run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 5 & exit 0".into()],
            &[],
            &[],
            None,
            Duration::from_millis(100),
        )
        .expect("process");
        assert_eq!(output.exit_code, 0);
        assert!(!output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_terminating_signal_separately_from_exit_code() {
        let output = run(
            Path::new("/bin/sh"),
            &["-c".into(), "kill -TERM $$".into()],
            &[],
            &[],
            None,
            Duration::from_secs(1),
        )
        .expect("process");
        assert_eq!(output.signal, Some(libc::SIGTERM));
        assert_eq!(output.exit_code, 1);
        assert!(!output.timed_out);
    }
}
