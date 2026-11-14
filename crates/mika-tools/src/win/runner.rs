use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::sys::{RunOutput, Runner};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ProcessRunner;

/// UTF-16LE from `cmd /U`: text in other encodings never holds a zero byte, while
/// UTF-16 of Latin and Cyrillic text has zeros in odd places (line breaks at least)
/// and none in even ones.
fn looks_utf16le(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && bytes.len().is_multiple_of(2)
        && bytes.iter().skip(1).step_by(2).any(|b| *b == 0)
        && !bytes.iter().step_by(2).any(|b| *b == 0)
}

/// UTF-16LE from `cmd /U`, UTF-8 when valid, otherwise the OEM code page that
/// console programs use (866 for Russian).
pub fn decode_console(bytes: &[u8]) -> String {
    if looks_utf16le(bytes) {
        let wide: Vec<u16> = bytes
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        return String::from_utf16_lossy(&wide);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    use windows::Win32::Globalization::{
        MultiByteToWideChar, CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS,
    };
    // SAFETY: first call measures, second writes into a buffer of that size.
    unsafe {
        let n = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if n <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; n as usize];
        MultiByteToWideChar(
            CP_OEMCP,
            MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
            bytes,
            Some(&mut wide),
        );
        String::from_utf16_lossy(&wide)
    }
}

/// A Windows job object that ends its processes when its last handle is
/// closed. This process holds the only handle and never closes it, so Windows
/// closes it when the process ends, however it ends: a program in the job
/// never outlives Mika. A screen capture needs this: an FFmpeg left behind
/// by a crash would go on recording with no mark of it on the screen and
/// with nobody to pause it on a password window.
///
/// The calls are declared here: the `windows` crate of the workspace is built
/// without its job object module.
mod job {
    use std::ffi::c_void;
    use std::os::windows::io::RawHandle;

    type Handle = *mut c_void;

    /// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct ExtendedLimits {
        basic: BasicLimits,
        /// `IO_COUNTERS`.
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    /// `JobObjectExtendedLimitInformation`.
    const EXTENDED_LIMITS: i32 = 9;
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: i32,
            info: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        #[cfg(test)]
        fn IsProcessInJob(process: Handle, job: Handle, result: *mut i32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    pub(super) struct KillJob(Handle);

    // SAFETY: a job handle is a kernel object; Windows lets any thread use it.
    unsafe impl Send for KillJob {}
    // SAFETY: as above; the calls on it take no Rust-side state.
    unsafe impl Sync for KillJob {}

    impl KillJob {
        pub(super) fn new() -> Option<Self> {
            let limits = ExtendedLimits {
                basic: BasicLimits {
                    limit_flags: KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: an unnamed job with default security; `limits` is the
            // structure the class asks for and lives through the call.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let set = SetInformationJobObject(
                    job,
                    EXTENDED_LIMITS,
                    (&limits as *const ExtendedLimits).cast(),
                    std::mem::size_of::<ExtendedLimits>() as u32,
                );
                if set == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }

        /// Puts the process into the job. False when Windows refuses.
        pub(super) fn take(&self, process: RawHandle) -> bool {
            // SAFETY: both handles are open; a refusal comes back as zero.
            unsafe { AssignProcessToJobObject(self.0, process.cast()) != 0 }
        }

        /// Whether the process is in this job.
        #[cfg(test)]
        pub(super) fn holds(&self, process: RawHandle) -> bool {
            let mut inside = 0;
            // SAFETY: both handles are open and `inside` outlives the call.
            unsafe { IsProcessInJob(process.cast(), self.0, &mut inside) != 0 && inside != 0 }
        }
    }

    impl Drop for KillJob {
        fn drop(&mut self) {
            // SAFETY: the handle is this value's own and is closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// The job of this process, made at the first use and kept to its end.
    pub(super) fn of_this_process() -> Option<&'static KillJob> {
        static JOB: std::sync::OnceLock<Option<KillJob>> = std::sync::OnceLock::new();
        JOB.get_or_init(KillJob::new).as_ref()
    }
}

/// Ties a started program to the life of this process: when Mika ends in any
/// way, Windows ends the program too.
fn tie_to_this_process(child: &tokio::process::Child) -> bool {
    match (job::of_this_process(), child.raw_handle()) {
        (Some(job), Some(process)) => job.take(process),
        _ => false,
    }
}

/// Kills the process and its children.
async fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

#[async_trait]
impl Runner for ProcessRunner {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<RunOutput, String> {
        let mut cmd = Command::new(program);
        if program.to_lowercase().ends_with("cmd.exe") {
            // cmd does not understand the standard argument quoting; pass the line as is.
            for a in args {
                cmd.raw_arg(a);
            }
        } else {
            cmd.args(args);
        }
        let mut child = cmd
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("не удалось запустить {program}: {e}"))?;
        let pid = child.id().unwrap_or(0);
        let mut out = child.stdout.take().expect("piped stdout");
        let mut err = child.stderr.take().expect("piped stderr");
        let read_out = async {
            let mut b = Vec::new();
            let _ = out.read_to_end(&mut b).await;
            b
        };
        let read_err = async {
            let mut b = Vec::new();
            let _ = err.read_to_end(&mut b).await;
            b
        };
        let work = async {
            let (o, e, status) = tokio::join!(read_out, read_err, child.wait());
            (o, e, status)
        };

        tokio::select! {
            (o, e, status) = work => Ok(RunOutput {
                code: status.ok().and_then(|s| s.code()),
                stdout: decode_console(&o),
                stderr: decode_console(&e),
                timed_out: false,
            }),
            _ = tokio::time::sleep(timeout) => {
                kill_tree(pid).await;
                Ok(RunOutput { timed_out: true, ..Default::default() })
            }
            _ = cancel.cancelled() => {
                kill_tree(pid).await;
                Err("остановлено".into())
            }
        }
    }

    async fn run_streaming(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        timeout: Duration,
        cancel: CancellationToken,
        on_stdout: &(dyn for<'l> Fn(&'l str) + Send + Sync),
    ) -> Result<RunOutput, String> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("не удалось запустить {program}: {e}"))?;
        let pid = child.id().unwrap_or(0);
        let out = child.stdout.take().expect("piped stdout");
        let mut err = child.stderr.take().expect("piped stderr");
        let read_out = async {
            let mut reader = tokio::io::BufReader::new(out);
            let mut tail = Tail::new(STDOUT_TAIL);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&line);
                        on_stdout(text.trim_end_matches(['\r', '\n']));
                        tail.push(&line);
                    }
                }
            }
            tail.into_bytes()
        };
        let read_err = async {
            let mut tail = Tail::new(STDERR_TAIL);
            let mut buf = vec![0u8; 8192];
            loop {
                match err.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => tail.push(&buf[..n]),
                }
            }
            tail.into_bytes()
        };
        let work = async {
            let (o, e, status) = tokio::join!(read_out, read_err, child.wait());
            (o, e, status)
        };

        tokio::select! {
            (o, e, status) = work => Ok(RunOutput {
                code: status.ok().and_then(|s| s.code()),
                stdout: decode_console(&o),
                stderr: decode_console(&e),
                timed_out: false,
            }),
            _ = tokio::time::sleep(timeout) => {
                kill_tree(pid).await;
                Ok(RunOutput { timed_out: true, ..Default::default() })
            }
            _ = cancel.cancelled() => {
                kill_tree(pid).await;
                Err("остановлено".into())
            }
        }
    }

    async fn run_controlled(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        timeout: Duration,
        cancel: CancellationToken,
        quit: CancellationToken,
        quit_wait: Duration,
        on_stdout: &(dyn for<'l> Fn(&'l str) + Send + Sync),
    ) -> Result<RunOutput, String> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("не удалось запустить {program}: {e}"))?;
        let pid = child.id().unwrap_or(0);
        // A program that runs until it is told to finish must not be left
        // running when nobody is there to tell it: without the tie it is
        // not run at all.
        if !tie_to_this_process(&child) {
            kill_tree(pid).await;
            return Err(format!(
                "не удалось связать {program} с приложением, поэтому он не запущен"
            ));
        }
        // Kept open to the end: a closed input reads as its end, not as `q`.
        let mut input = child.stdin.take();
        let out = child.stdout.take().expect("piped stdout");
        let mut err = child.stderr.take().expect("piped stderr");
        let read_out = async {
            let mut reader = tokio::io::BufReader::new(out);
            let mut tail = Tail::new(STDOUT_TAIL);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&line);
                        on_stdout(text.trim_end_matches(['\r', '\n']));
                        tail.push(&line);
                    }
                }
            }
            tail.into_bytes()
        };
        let read_err = async {
            let mut tail = Tail::new(STDERR_TAIL);
            let mut buf = vec![0u8; 8192];
            loop {
                match err.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => tail.push(&buf[..n]),
                }
            }
            tail.into_bytes()
        };
        let work = async {
            let (o, e, status) = tokio::join!(read_out, read_err, child.wait());
            RunOutput {
                code: status.ok().and_then(|s| s.code()),
                stdout: decode_console(&o),
                stderr: decode_console(&e),
                timed_out: false,
            }
        };
        tokio::pin!(work);
        let limit = tokio::time::sleep(timeout);
        tokio::pin!(limit);

        tokio::select! {
            out = &mut work => return Ok(out),
            _ = &mut limit => {
                kill_tree(pid).await;
                return Ok(RunOutput { timed_out: true, ..Default::default() });
            }
            _ = cancel.cancelled() => {
                kill_tree(pid).await;
                return Err("остановлено".into());
            }
            _ = quit.cancelled() => {}
        }
        // Asked to finish: `q` goes to its input, and it ends by itself.
        if let Some(pipe) = input.as_mut() {
            let _ = pipe.write_all(b"q\n").await;
            let _ = pipe.flush().await;
        }
        tokio::select! {
            out = &mut work => Ok(out),
            _ = tokio::time::sleep(quit_wait) => {
                log::warn!(
                    target: "jobs",
                    "a program did not end when asked and was killed, what it wrote may be cut: waited_ms={}",
                    quit_wait.as_millis()
                );
                kill_tree(pid).await;
                // Killed, its pipes close; a child of it may still hold them.
                match tokio::time::timeout(KILLED_WAIT, &mut work).await {
                    Ok(out) => Ok(out),
                    Err(_) => Ok(RunOutput::default()),
                }
            }
            _ = &mut limit => {
                kill_tree(pid).await;
                Ok(RunOutput { timed_out: true, ..Default::default() })
            }
            _ = cancel.cancelled() => {
                kill_tree(pid).await;
                Err("остановлено".into())
            }
        }
    }

    fn run_blocking(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<RunOutput, String> {
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        let mut child = std::process::Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("не удалось запустить {program}: {e}"))?;
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut b = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut b);
                }
                b
            })
        };
        let out = drain(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let err = drain(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let started = std::time::Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let Some(status) = status else {
            // A child of the program may still hold the pipes: their readers
            // are left to end by themselves.
            return Ok(RunOutput {
                timed_out: true,
                ..Default::default()
            });
        };
        let o = out.join().unwrap_or_default();
        let e = err.join().unwrap_or_default();
        Ok(RunOutput {
            code: status.code(),
            stdout: decode_console(&o),
            stderr: decode_console(&e),
            timed_out: false,
        })
    }
}

/// Longest wait for the output of a program that was just killed.
const KILLED_WAIT: Duration = Duration::from_secs(5);

/// Last bytes kept of a streamed output.
const STDOUT_TAIL: usize = 64 * 1024;
const STDERR_TAIL: usize = 8 * 1024 * 1024;

/// The last `cap` bytes of a stream, cut at a character boundary.
struct Tail {
    cap: usize,
    bytes: Vec<u8>,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            bytes: Vec::new(),
        }
    }

    fn push(&mut self, more: &[u8]) {
        self.bytes.extend_from_slice(more);
        // Trims seldom: only when twice the limit is held.
        if self.bytes.len() > self.cap * 2 {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.bytes.len() <= self.cap {
            return;
        }
        let mut cut = self.bytes.len() - self.cap;
        while cut < self.bytes.len() && (0x80..0xC0).contains(&self.bytes[cut]) {
            cut += 1;
        }
        self.bytes.drain(..cut);
    }

    fn into_bytes(mut self) -> Vec<u8> {
        self.trim();
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_from_cmd_utf8_and_plain_text_decode() {
        let wide: Vec<u8> = "привет\r\nok\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode_console(&wide), "привет\r\nok\r\n");
        assert_eq!(decode_console("привет".as_bytes()), "привет");
        assert_eq!(decode_console(b"ok\r\n"), "ok\r\n");
        assert!(!looks_utf16le(b"abcd"));
    }

    #[test]
    fn tail_keeps_the_last_bytes_at_a_character_boundary() {
        let mut t = Tail::new(5);
        t.push("ab".as_bytes());
        t.push("вгд".as_bytes());
        // "abвгд" is 2 + 6 bytes; the last 5 start inside "в".
        assert_eq!(String::from_utf8(t.into_bytes()).unwrap(), "гд");
    }

    #[tokio::test]
    async fn streaming_gives_lines_and_runs_in_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let lines = parking_lot::Mutex::new(Vec::<String>::new());
        let out = ProcessRunner
            .run_streaming(
                "cmd.exe",
                &["/c".into(), "echo a& echo b& cd".into()],
                Some(dir.path()),
                Duration::from_secs(10),
                CancellationToken::new(),
                &|l: &str| lines.lock().push(l.trim().to_string()),
            )
            .await
            .unwrap();
        assert_eq!(out.code, Some(0));
        let lines = lines.into_inner();
        assert_eq!(&lines[..2], ["a", "b"]);
        let name = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(lines[2].contains(&name), "{lines:?}");
    }

    #[test]
    fn the_job_limits_have_the_size_windows_expects() {
        // 144 bytes on 64-bit Windows, 112 on 32-bit: a wrong layout would
        // make Windows refuse the limit.
        let expected = if cfg!(target_pointer_width = "64") {
            144
        } else {
            112
        };
        assert_eq!(std::mem::size_of::<super::job::ExtendedLimits>(), expected);
    }

    #[test]
    fn a_program_in_the_job_ends_when_the_job_is_closed() {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        // What happens to a capture when Mika ends by force: Windows closes
        // the job handle of the dead process, here it is closed by hand.
        let job = super::job::KillJob::new().expect("a job object");
        let mut child = std::process::Command::new("cmd.exe")
            // Waits for a line on its input, which stays open and silent.
            .args(["/q", "/k"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(!job.holds(child.as_raw_handle()));
        assert!(job.take(child.as_raw_handle()));
        assert!(job.holds(child.as_raw_handle()));
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            child.try_wait().unwrap().is_none(),
            "it runs while the job is open"
        );
        drop(job);
        let started = std::time::Instant::now();
        let ended = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if started.elapsed() > Duration::from_secs(15) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if !ended {
            let _ = child.kill();
        }
        assert!(ended, "closing the job ends the program");
    }

    #[tokio::test]
    async fn a_controlled_program_is_tied_to_this_process() {
        let mut child = Command::new("cmd.exe")
            // Waits for a line on its input, which stays open and silent.
            .args(["/q", "/k"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let job = super::job::of_this_process().expect("the job of this process");
        let handle = child.raw_handle().unwrap();
        assert!(!job.holds(handle));
        assert!(super::tie_to_this_process(&child));
        assert!(job.holds(handle));
        let _ = child.kill().await;
    }

    /// Reads one line from its input and prints it back. `!x!` is read when
    /// the line runs, `%x%` would be read before it.
    fn echo_input() -> Vec<String> {
        vec![
            "/v:on".into(),
            "/c".into(),
            "echo ready& set /p x=& echo got !x!".into(),
        ]
    }

    #[tokio::test]
    async fn a_controlled_run_is_told_to_quit_through_its_input() {
        let lines = parking_lot::Mutex::new(Vec::<String>::new());
        let quit = CancellationToken::new();
        let asked = quit.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            asked.cancel();
        });
        let started = std::time::Instant::now();
        let out = ProcessRunner
            .run_controlled(
                "cmd.exe",
                &echo_input(),
                None,
                Duration::from_secs(30),
                CancellationToken::new(),
                quit,
                Duration::from_secs(10),
                &|l: &str| lines.lock().push(l.trim().to_string()),
            )
            .await
            .unwrap();
        assert_eq!(out.code, Some(0), "it ended by itself: {out:?}");
        assert!(!out.timed_out);
        assert!(out.stdout.contains("got q"), "{out:?}");
        assert_eq!(*lines.lock(), ["ready", "got q"]);
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[tokio::test]
    async fn a_controlled_run_that_ignores_quit_is_killed() {
        let quit = CancellationToken::new();
        quit.cancel();
        let started = std::time::Instant::now();
        let out = ProcessRunner
            .run_controlled(
                "ping.exe",
                &["-n".into(), "30".into(), "127.0.0.1".into()],
                None,
                Duration::from_secs(60),
                CancellationToken::new(),
                quit,
                Duration::from_millis(300),
                &|_: &str| {},
            )
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_ne!(out.code, Some(0), "it was killed: {out:?}");
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn a_controlled_run_ends_by_itself_stops_and_times_out() {
        // It ends before anyone asks.
        let out = ProcessRunner
            .run_controlled(
                "cmd.exe",
                &["/c".into(), "echo hi".into()],
                None,
                Duration::from_secs(30),
                CancellationToken::new(),
                CancellationToken::new(),
                Duration::from_secs(1),
                &|_: &str| {},
            )
            .await
            .unwrap();
        assert_eq!((out.code, out.stdout.trim()), (Some(0), "hi"));

        // «Стоп» kills it at once, asked to quit or not.
        let cancel = CancellationToken::new();
        cancel.cancel();
        let r = ProcessRunner
            .run_controlled(
                "cmd.exe",
                &echo_input(),
                None,
                Duration::from_secs(30),
                cancel,
                CancellationToken::new(),
                Duration::from_secs(10),
                &|_: &str| {},
            )
            .await;
        assert_eq!(r, Err("остановлено".to_string()));

        let out = ProcessRunner
            .run_controlled(
                "cmd.exe",
                &echo_input(),
                None,
                Duration::from_millis(300),
                CancellationToken::new(),
                CancellationToken::new(),
                Duration::from_secs(10),
                &|_: &str| {},
            )
            .await
            .unwrap();
        assert!(out.timed_out);
    }

    #[test]
    fn blocking_run_returns_output_and_stops_at_the_timeout() {
        let out = ProcessRunner
            .run_blocking(
                "cmd.exe",
                &["/c".into(), "echo hi".into()],
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!((out.code, out.stdout.trim()), (Some(0), "hi"));
        let started = std::time::Instant::now();
        let out = ProcessRunner
            .run_blocking(
                "ping.exe",
                &["-n".into(), "30".into(), "127.0.0.1".into()],
                Duration::from_millis(300),
            )
            .unwrap();
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
