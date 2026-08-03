use std::process::Stdio;

use crate::{Error, Result};

/// Kills one command's process group when execution completes or is cancelled.
///
/// The command must be configured with `process_group(0)` before it is spawned.
pub struct ProcessGroupGuard {
    id: u32,
    armed: bool,
}

impl ProcessGroupGuard {
    /// Arms cleanup for the child's process group.
    pub fn new(child: &tokio::process::Child) -> Result<Self> {
        let id = child
            .id()
            .ok_or_else(|| Error::Sandbox("command process ID unavailable".into()))?;
        Ok(Self { id, armed: true })
    }

    /// Kills the process group once.
    pub fn kill(&mut self) {
        if self.armed {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.id)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            self.armed = false;
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}
