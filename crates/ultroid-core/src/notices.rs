//! Error notices — port of `pyUltroid/_core/notices.py`
//! (`sanitize_error_text`, error-notice delivery to LOG_CHANNEL).

/// Strip secrets / tokens from error text before it is shown or logged.
pub fn sanitize_error_text(text: &str) -> String {
    let mut out = text.to_string();
    for key in ["BOT_TOKEN", "API_HASH", "api_hash", "password", "token"] {
        // crude redaction: `key=VALUE` / `key: VALUE` / `"key": "VALUE"`
        let mut start = 0;
        while let Some(pos) = out[start..].find(key) {
            let abs = start + pos;
            // find the value start after = or :
            let rest = &out[abs + key.len()..];
            let mut idx = 0;
            for c in rest.chars() {
                if c == '=' || c == ':' || c == '"' || c == '\'' || c.is_whitespace() {
                    idx += c.len_utf8();
                } else {
                    break;
                }
            }
            let val_start = abs + key.len() + idx;
            let val_end = out[val_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
                .map(|i| val_start + i)
                .unwrap_or(out.len());
            if val_end > val_start {
                out.replace_range(val_start..val_end, "[redacted]");
                start = val_start + "[redacted]".len();
            } else {
                start = abs + key.len();
            }
        }
    }
    out
}

/// Short one-line error notice, as delivered to the log channel.
pub fn error_notice(context: &str, err: &dyn std::fmt::Display) -> String {
    format!("⚠️ {context}\n`{}`", sanitize_error_text(&err.to_string()))
}
