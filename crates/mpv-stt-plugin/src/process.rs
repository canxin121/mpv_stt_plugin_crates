use mpv_stt_common::{MpvSttError, Result};
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

fn format_cmd_for_error(label: &str) -> String {
    label.to_string()
}

pub fn run_capture_output(mut cmd: Command, label: &str, timeout: Duration) -> Result<Output> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        MpvSttError::ProcessFailed(format!(
            "Failed to spawn {}: {}",
            format_cmd_for_error(label),
            e
        ))
    })?;

    match child.wait_timeout(timeout).map_err(|e| {
        MpvSttError::ProcessFailed(format!(
            "Failed waiting for {}: {}",
            format_cmd_for_error(label),
            e
        ))
    })? {
        Some(_) => {
            let output = child.wait_with_output()?;
            Ok(output)
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(MpvSttError::ProcessTimeout(format!(
                "{} timed out after {}ms",
                format_cmd_for_error(label),
                timeout.as_millis()
            )))
        }
    }
}

pub fn run_capture_output_with_stdin(
    mut cmd: Command,
    label: &str,
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<Output> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        MpvSttError::ProcessFailed(format!(
            "Failed to spawn {}: {}",
            format_cmd_for_error(label),
            e
        ))
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(stdin_bytes)?;
    }

    match child.wait_timeout(timeout).map_err(|e| {
        MpvSttError::ProcessFailed(format!(
            "Failed waiting for {}: {}",
            format_cmd_for_error(label),
            e
        ))
    })? {
        Some(_) => {
            let output = child.wait_with_output()?;
            Ok(output)
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(MpvSttError::ProcessTimeout(format!(
                "{} timed out after {}ms",
                format_cmd_for_error(label),
                timeout.as_millis()
            )))
        }
    }
}
