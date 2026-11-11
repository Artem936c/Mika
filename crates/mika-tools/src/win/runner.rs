use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::sys::{RunOutput, Runner};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct ProcessRunner;

/// UTF-8 when valid, otherwise the OEM code page that console programs use (866 for Russian).
pub fn decode_console(bytes: &[u8]) -> String {
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
}
