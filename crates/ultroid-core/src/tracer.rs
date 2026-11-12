//! Code tracer — port of `pyUltroid/tracer/` (`_indexer.py`, `_tracker.py`,
//! `_searcher.py`). Indexes the Rust source tree and answers
//! "where is X defined / who calls X" with plain grep-style search.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TraceHit {
    pub file: PathBuf,
    pub line: usize,
    pub text: String,
}

pub struct Tracer {
    pub root: PathBuf,
}

impl Tracer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Tracer { root: root.into() }
    }

    fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().map_or(false, |n| n == "target" || n == ".git") {
                        continue;
                    }
                    Self::walk_rs(&p, out);
                } else if p.extension().map_or(false, |x| x == "rs") {
                    out.push(p);
                }
            }
        }
    }

    /// Find definitions/usages of `symbol` across the tree.
    pub fn search(&self, symbol: &str) -> Vec<TraceHit> {
        let mut files = Vec::new();
        Self::walk_rs(&self.root, &mut files);
        let mut hits = Vec::new();
        for f in files {
            if let Ok(text) = std::fs::read_to_string(&f) {
                for (i, line) in text.lines().enumerate() {
                    if line.contains(symbol) {
                        hits.push(TraceHit {
                            file: f.clone(),
                            line: i + 1,
                            text: line.trim().to_string(),
                        });
                    }
                }
            }
        }
        hits
    }

    /// Definitions of `symbol` (`fn symbol`, `struct symbol`, …).
    pub fn definitions(&self, symbol: &str) -> Vec<TraceHit> {
        self.search(symbol)
            .into_iter()
            .filter(|h| {
                h.text.starts_with(&format!("pub fn {symbol}"))
                    || h.text.starts_with(&format!("fn {symbol}"))
                    || h.text.starts_with(&format!("pub struct {symbol}"))
                    || h.text.starts_with(&format!("struct {symbol}"))
                    || h.text.starts_with(&format!("pub enum {symbol}"))
                    || h.text.starts_with(&format!("pub mod {symbol}"))
            })
            .collect()
    }
}
