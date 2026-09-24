//! Running external programs.
//!
//! Every external program the app invokes goes through here, including the ones
//! named by user actions. Two properties matter:
//!
//! * **No shell.** Commands are argument vectors. Clipboard content is untrusted
//!   input, and passing it through `sh -c` would make any copied text a command
//!   injection.
//! * **No deadlocks or hangs.** stdin, stdout and stderr are pumped on separate
//!   threads so a full pipe cannot wedge the caller, and every run is bounded by
//!   a timeout so a misconfigured command cannot freeze the tray.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// What a finished command produced.
#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl CommandOutput {
    /// Standard output as trimmed text.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// Why an external command did not produce a usable result.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("action has an empty command")]
    Empty,
    #[error("could not start `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program}` did not finish within {}s and was stopped", timeout.as_secs())]
    Timeout { program: String, timeout: Duration },
    #[error("`{program}` failed ({status}): {stderr}")]
    Failed {
        program: String,
        status: String,
        stderr: String,
    },
}

/// Runs `argv`, optionally writing `input` to its standard input, and captures
/// what it produced.
///
/// # Errors
///
/// Returns [`ExecError::Spawn`] if the program could not be started (typically
/// not installed), [`ExecError::Timeout`] if it outlived `timeout`, and
/// [`ExecError::Failed`] for a non-zero exit, carrying stderr so the
/// notification can say what went wrong.
pub fn run(
    argv: &[String],
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandOutput, ExecError> {
    let (program, arguments) = argv.split_first().ok_or(ExecError::Empty)?;

    let mut child = Command::new(program)
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| ExecError::Spawn {
            program: program.clone(),
            source,
        })?;

    // Each stream gets its own thread. Writing a large image to stdin while the
    // child is writing to stdout would otherwise deadlock as soon as either
    // pipe buffer fills.
    let writer = child.stdin.take().map(|mut pipe| {
        let payload = input.unwrap_or_default().to_vec();
        std::thread::spawn(move || {
            // A command that exits without reading everything (`head`-like) gives
            // a broken pipe here; that is the command's prerogative, not an error.
            let _ = pipe.write_all(&payload);
        })
    });
    let out_reader = child.stdout.take().map(drain);
    let err_reader = child.stderr.take().map(drain);

    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExecError::Timeout {
                program: program.clone(),
                timeout,
            });
        }
        Err(source) => {
            return Err(ExecError::Spawn {
                program: program.clone(),
                source,
            })
        }
    };

    if let Some(handle) = writer {
        let _ = handle.join();
    }
    let stdout = out_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr_bytes = err_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();

    if !status.success() {
        return Err(ExecError::Failed {
            program: program.clone(),
            status: status
                .code()
                .map_or_else(|| "killed by signal".to_string(), |c| c.to_string()),
            stderr: if stderr.is_empty() {
                "no error output".to_string()
            } else {
                stderr
            },
        });
    }

    Ok(CommandOutput { stdout, stderr })
}

/// Reads a pipe to end on its own thread.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Replaces `{name}` placeholders in each argument.
///
/// Substitution is per-argument and never splits an argument into several, so a
/// value containing spaces stays one argument and cannot inject extra ones.
#[must_use]
pub fn substitute(argv: &[String], vars: &[(&str, String)]) -> Vec<String> {
    argv.iter()
        .map(|arg| {
            let mut out = arg.clone();
            for (name, value) in vars {
                let needle = format!("{{{name}}}");
                if out.contains(&needle) {
                    out = out.replace(&needle, value);
                }
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    const QUICK: Duration = Duration::from_secs(10);

    #[test]
    fn captures_standard_output() {
        let out = run(&argv(&["echo", "hello"]), None, QUICK).expect("echo runs");
        assert_eq!(out.stdout_text(), "hello");
    }

    #[test]
    fn feeds_standard_input() {
        let out = run(&argv(&["cat"]), Some(b"round trip"), QUICK).expect("cat runs");
        assert_eq!(out.stdout_text(), "round trip");
    }

    #[test]
    fn handles_binary_input_without_corrupting_it() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let out = run(&argv(&["cat"]), Some(&payload), QUICK).expect("cat runs");
        assert_eq!(out.stdout, payload);
    }

    #[test]
    fn a_large_payload_does_not_deadlock() {
        // Big enough to exceed any pipe buffer in both directions at once, which
        // is what the separate reader and writer threads exist to survive.
        let payload = vec![b'x'; 8 * 1024 * 1024];
        let out = run(&argv(&["cat"]), Some(&payload), Duration::from_secs(30)).expect("cat runs");
        assert_eq!(out.stdout.len(), payload.len());
    }

    #[test]
    fn a_nonzero_exit_is_an_error_carrying_stderr() {
        let err = run(&argv(&["sh", "-c", "echo boom >&2; exit 3"]), None, QUICK)
            .expect_err("should fail");
        match err {
            ExecError::Failed { status, stderr, .. } => {
                assert_eq!(status, "3");
                assert_eq!(stderr, "boom");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_program_is_reported_by_name() {
        let err = run(&argv(&["lcc-does-not-exist"]), None, QUICK).expect_err("no such program");
        match err {
            ExecError::Spawn { program, .. } => assert_eq!(program, "lcc-does-not-exist"),
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn a_hanging_command_is_killed_at_the_timeout() {
        let started = std::time::Instant::now();
        let err = run(&argv(&["sleep", "30"]), None, Duration::from_millis(300))
            .expect_err("should time out");
        assert!(matches!(err, ExecError::Timeout { .. }), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout did not stop the command promptly"
        );
    }

    #[test]
    fn an_empty_command_is_an_error_not_a_panic() {
        assert!(matches!(run(&[], None, QUICK), Err(ExecError::Empty)));
    }

    #[test]
    fn a_command_that_ignores_stdin_does_not_fail_on_a_broken_pipe() {
        let out = run(&argv(&["true"]), Some(&vec![b'x'; 1024 * 1024]), QUICK)
            .expect("a broken pipe is the command's choice, not an error");
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn substitution_fills_placeholders() {
        let template = argv(&["magick", "{input}", "-resize", "{geometry}", "{output}"]);
        let filled = substitute(
            &template,
            &[
                ("input", "/tmp/a.png".to_string()),
                ("geometry", "512x512".to_string()),
                ("output", "/tmp/b.png".to_string()),
            ],
        );
        assert_eq!(
            filled,
            argv(&["magick", "/tmp/a.png", "-resize", "512x512", "/tmp/b.png"])
        );
    }

    #[test]
    fn a_substituted_value_stays_a_single_argument() {
        // A path with spaces must not become two arguments.
        let filled = substitute(
            &argv(&["cat", "{input}"]),
            &[("input", "/tmp/a b.png".to_string())],
        );
        assert_eq!(filled.len(), 2);
        assert_eq!(filled[1], "/tmp/a b.png");
    }

    #[test]
    fn an_unknown_placeholder_is_left_alone() {
        let filled = substitute(&argv(&["x", "{nope}"]), &[("input", "v".to_string())]);
        assert_eq!(filled[1], "{nope}");
    }

    #[test]
    fn clipboard_content_is_never_interpreted_as_a_command() {
        // The whole point of using an argv rather than a shell string: this text
        // must arrive at the program verbatim.
        let hostile = "; rm -rf / #$(whoami)`id`";
        let out = run(&argv(&["cat"]), Some(hostile.as_bytes()), QUICK).expect("cat runs");
        assert_eq!(out.stdout_text(), hostile);
    }
}
