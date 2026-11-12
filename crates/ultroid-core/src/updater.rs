//! Self-updater — port of `pyUltroid/updater/` (`_engine.py`, `_git.py`,
//! `_shadow.py`). Uses the `git` CLI (no GitPython in Rust); updates only
//! when the working tree is clean, mirroring the Python safety gate.

use crate::error::{Error, Result};

pub struct Updater {
    pub repo_dir: std::path::PathBuf,
    pub remote: String,
    pub branch: String,
}

impl Updater {
    pub fn new(repo_dir: impl Into<std::path::PathBuf>) -> Self {
        Updater {
            repo_dir: repo_dir.into(),
            remote: "origin".to_string(),
            branch: "main".to_string(),
        }
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&self.repo_dir)
            .output()?;
        if !out.status.success() {
            return Err(Error::Other(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.git(&["status", "--porcelain"])?.is_empty())
    }

    pub fn current_commit(&self) -> Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Fetch + fast-forward only. Refuses when the tree is dirty (Python's
    /// dirty gate, relaxed to OpenClaw's rule only on explicit force).
    pub fn update(&self, force: bool) -> Result<String> {
        if !force && !self.is_clean()? {
            return Err(Error::Other(
                "working tree is dirty; refusing to update (pass force to override)".into(),
            ));
        }
        self.git(&["fetch", &self.remote])?;
        let before = self.current_commit()?;
        self.git(&["merge", "--ff-only", &format!("{}/{}", self.remote, self.branch)])?;
        let after = self.current_commit()?;
        if before == after {
            Ok("already up to date".to_string())
        } else {
            Ok(format!("updated {before} -> {after}"))
        }
    }
}
