//! Execute a control action on the VPS over SSH. Blocking — call from a
//! background thread and hand the result back over a channel.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub ok: bool,
    pub output: String,
}

/// Run `cmd` on the host via `ssh <host> bash -lc '<cmd>'`, capturing combined
/// stdout+stderr. The command text is passed on stdin so quoting never fights
/// the three shell layers.
pub fn run(host_alias: &str, cmd: &str) -> ActionResult {
    let mut child = match Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .arg(host_alias)
        .arg("bash -lc 'cat | bash -s'")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ActionResult { ok: false, output: format!("spawn ssh: {e}") }
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // Strip CR so a Windows-side string never trips bash on \r.
        let script = cmd.replace('\r', "");
        let _ = stdin.write_all(script.as_bytes());
    }

    match child.wait_with_output() {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&err);
            }
            if s.trim().is_empty() {
                s = "(no output)".into();
            }
            ActionResult { ok: out.status.success(), output: s }
        }
        Err(e) => ActionResult { ok: false, output: format!("ssh wait: {e}") },
    }
}
