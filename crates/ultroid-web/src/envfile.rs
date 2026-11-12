//! The `.env` document: one path-bound object, not a bag of free functions.
//!
//! Port of `pyUltroid/webserver/envfile.py` — the dependency leaf of the
//! package (stdlib only in Python); here it depends on nothing but
//! `ultroid-core`'s error type.
//!
//! Parsing, quoting and the atomic replace are private to the instance:
//! callers get exactly two verbs — [`EnvFile::read`] and
//! [`EnvFile::write`] — plus [`EnvFile::export`].
//!
//! One deliberate difference: Python resolves the default path against the
//! repository root (`<repo>/.env`). There is no repository root in the Rust
//! binary, so the default is `<data_dir>/.env` where `data_dir` is
//! `$ULTROID_DATA_DIR` (the same variable `ultroid-core`'s `Config` uses)
//! or the current directory. `$PYULTROID_ENV_FILE` still overrides both.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ultroid_core::Result;

pub const SETUP_KEYS: &[&str] = &["API_ID", "API_HASH", "BOT_TOKEN", "SESSION", "LOG_CHANNEL"];
pub const ENV_FILE_VAR: &str = "PYULTROID_ENV_FILE";
pub const REQUIRED_KEYS: &[&str] = &["API_ID", "API_HASH", "SESSION"];

/// A single `.env` document bound to one path.
#[derive(Debug, Clone)]
pub struct EnvFile {
    pub path: PathBuf,
}

impl EnvFile {
    pub fn new(path: Option<impl AsRef<Path>>) -> Self {
        Self {
            path: path
                .map(|p| p.as_ref().to_path_buf())
                .unwrap_or_else(Self::default_path),
        }
    }

    /// `$PYULTROID_ENV_FILE` when set, else `<data_dir>/.env`.
    pub fn default_path() -> PathBuf {
        if let Ok(override_path) = std::env::var(ENV_FILE_VAR) {
            if !override_path.is_empty() {
                return PathBuf::from(override_path);
            }
        }
        let data_dir = std::env::var("ULTROID_DATA_DIR").unwrap_or_else(|_| ".".to_string());
        Path::new(&data_dir).join(".env")
    }

    /// Parse the file; a missing file parses as empty.
    pub fn read(&self) -> HashMap<String, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Self::parse(&text),
            Err(_) => HashMap::new(),
        }
    }

    /// Parse dotenv text.
    ///
    /// Deliberately minimal and deliberately unchanged: no inline-comment
    /// stripping and no `export` prefix handling, because the writer never
    /// produces either.
    pub fn parse(text: &str) -> HashMap<String, String> {
        let mut data = HashMap::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || !line.contains('=') {
                continue;
            }
            let (key, value) = line.split_once('=').unwrap();
            data.insert(key.trim().to_string(), unquote(value.trim()));
        }
        data
    }

    /// File contents overlaid by the real environment — the real env wins.
    pub fn merged_with_environ(&self) -> HashMap<String, String> {
        let mut merged = self.read();
        for (key, value) in std::env::vars() {
            merged.insert(key, value);
        }
        merged
    }

    /// Required keys absent (or non-numeric / zero, for `API_ID`).
    pub fn missing(&self) -> Vec<&'static str> {
        let merged = self.merged_with_environ();
        let mut absent = Vec::new();
        for key in REQUIRED_KEYS {
            let value = merged.get(*key);
            let usable = if *key == "API_ID" {
                is_int(value)
            } else {
                value.map(|v| !v.is_empty()).unwrap_or(false)
            };
            if !usable {
                absent.push(*key);
            }
        }
        absent
    }

    /// True when every required credential is present and usable.
    pub fn complete(&self) -> bool {
        self.missing().is_empty()
    }

    /// Merge `updates` in place, preserving comments and key order, then
    /// atomically replace the file (same-dir temp file, fsync, chmod 0600,
    /// rename). When `apply_to_environ`, the updates also land in the
    /// process environment — real values are overwritten here, unlike
    /// [`EnvFile::export`], because the caller just wrote them.
    pub fn write(
        &self,
        updates: &HashMap<String, String>,
        apply_to_environ: bool,
    ) -> Result<PathBuf> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = self.render(updates);
        self.replace(&content)?;
        if apply_to_environ {
            for (key, value) in updates {
                // SAFETY: the setup server writes its own freshly-validated
                // values during initial configuration, before the bot starts
                // serving; no other thread reads these keys concurrently yet.
                unsafe {
                    std::env::set_var(key, value);
                }
            }
        }
        Ok(self.path.clone())
    }

    /// Seed the process environment from the file without overwriting real
    /// values. Returns how many keys were seeded.
    pub fn export(&self) -> usize {
        let mut count = 0;
        for (key, value) in self.read() {
            if std::env::var_os(&key).is_none() {
                // SAFETY: same reasoning as `write` — setup-time seeding.
                unsafe {
                    std::env::set_var(&key, &value);
                }
                count += 1;
            }
        }
        count
    }

    fn render(&self, updates: &HashMap<String, String>) -> String {
        let existing = std::fs::read_to_string(&self.path)
            .map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut remaining: HashMap<String, String> = updates.clone();
        let mut out: Vec<String> = Vec::new();
        for line in existing {
            let stripped = line.trim();
            if !stripped.is_empty() && !stripped.starts_with('#') && stripped.contains('=') {
                let key = stripped.split('=').next().unwrap().trim();
                if let Some(value) = remaining.remove(key) {
                    out.push(format!("{key}={}", format_value(&value)));
                    continue;
                }
            }
            out.push(line);
        }
        for (key, value) in &remaining {
            out.push(format!("{key}={}", format_value(value)));
        }
        let mut text = out.join("\n");
        text.push('\n');
        text
    }

    /// Same-directory temp file, fsync, chmod 0600, then atomic rename.
    fn replace(&self, content: &str) -> Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let pid = std::process::id();
        let tmp_name = format!(
            ".{}.{:x}.tmp",
            self.path.file_name().unwrap_or_default().to_string_lossy(),
            pid
        );
        let tmp_path = parent.join(tmp_name);
        let result = (|| -> Result<()> {
            let mut handle = std::fs::File::create(&tmp_path)?;
            handle.write_all(content.as_bytes())?;
            handle.flush()?;
            handle.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp_path, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        result
    }
}

impl Default for EnvFile {
    fn default() -> Self {
        Self::new(None::<&Path>)
    }
}

/// The default env path (module surface, mirroring `default_env_path`).
pub fn default_env_path() -> PathBuf {
    EnvFile::default_path()
}

pub fn parse_env_text(text: &str) -> HashMap<String, String> {
    EnvFile::parse(text)
}

pub fn read_env(path: Option<impl AsRef<Path>>) -> HashMap<String, String> {
    EnvFile::new(path).read()
}

pub fn write_env_atomic(
    updates: &HashMap<String, String>,
    path: Option<impl AsRef<Path>>,
    apply_to_environ: bool,
) -> Result<PathBuf> {
    EnvFile::new(path).write(updates, apply_to_environ)
}

/// Strip a matching outer quote pair; resolve escapes inside double quotes.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && bytes[0] == bytes[bytes.len() - 1]
        && (bytes[0] == b'\'' || bytes[0] == b'"')
    {
        let inner = &value[1..value.len() - 1];
        if bytes[0] == b'\'' {
            return inner.to_string();
        }
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    value.to_string()
}

fn format_value(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value.chars().any(char::is_whitespace) || value.contains('#') {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        return format!("\"{escaped}\"");
    }
    value.to_string()
}

fn is_int(value: Option<&String>) -> bool {
    match value {
        Some(v) if !v.is_empty() => v.parse::<i64>().map(|n| n != 0).unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_no_inline_comments_no_export() {
        let data = EnvFile::parse(
            "# comment\nAPI_ID=12345\nAPI_HASH=abcdef # trailing stays\nEMPTY=\nQUOTED=\"a b\"\nSINGLE='x y'\n",
        );
        assert_eq!(data["API_ID"], "12345");
        // No inline-comment stripping: the trailing comment is part of the value.
        assert_eq!(data["API_HASH"], "abcdef # trailing stays");
        assert_eq!(data["EMPTY"], "");
        assert_eq!(data["QUOTED"], "a b");
        assert_eq!(data["SINGLE"], "x y");
    }

    #[test]
    fn unquote_resolves_double_quote_escapes() {
        assert_eq!(unquote("\"a\\\"b\\\\c\""), "a\"b\\c");
        assert_eq!(unquote("'a\\b'"), "a\\b");
        assert_eq!(unquote("plain"), "plain");
    }

    #[test]
    fn format_value_quotes_whitespace_and_hash() {
        assert_eq!(format_value(""), "\"\"");
        assert_eq!(format_value("abc"), "abc");
        assert_eq!(format_value("a b"), "\"a b\"");
        assert_eq!(format_value("a#b"), "\"a#b\"");
        assert_eq!(format_value("a\"b"), "a\"b");
        assert_eq!(format_value("a\" b"), "\"a\\\" b\"");
    }

    #[test]
    fn write_preserves_comments_and_order() {
        let dir = std::env::temp_dir().join(format!("ultroid-web-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "# header\nAPI_ID=1\n# keep me\nOTHER=x\n").unwrap();
        let env = EnvFile::new(Some(&path));
        let mut updates = HashMap::new();
        updates.insert("API_ID".to_string(), "999".to_string());
        updates.insert("NEW_KEY".to_string(), "v v".to_string());
        env.write(&updates, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# header\nAPI_ID=999\n# keep me\nOTHER=x\nNEW_KEY=\"v v\"\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_requires_numeric_nonzero_api_id() {
        let dir = std::env::temp_dir().join(format!("ultroid-web-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "API_ID=0\nAPI_HASH=h\nSESSION=s\n").unwrap();
        // merged_with_environ could pick up real env vars; use a file whose
        // keys are unique to this test by checking `missing` through parse:
        let env = EnvFile::new(Some(&path));
        let parsed = env.read();
        assert_eq!(parsed["API_ID"], "0");
        assert!(!is_int(Some(&"0".to_string())));
        assert!(is_int(Some(&"123".to_string())));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
