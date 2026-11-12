//! Port of `plugins/usage.py` — host and database usage.
//!
//! `.usage` — disk / CPU / memory / network usage from `/proc` and
//! `df` (real; the Python version used `psutil`). `.usage db` — DB
//! usage measured by walking the stored keys (real; the Python version
//! reported a fixed per-backend quota, which this port does not invent).
//! `.usage heroku` — Heroku quota via the Heroku API when
//! `HEROKU_API` + `HEROKU_APP_NAME` are configured (real HTTP);
//! otherwise it says so plainly.

use crate::util::{def, reg_add};
use ultroid_core::fns::humanbytes;
use ultroid_core::{Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "usage",
            "usage",
            "Host usage: `.usage`, `.usage db`, `.usage heroku`.",
        ),
        |ctx| Box::pin(usage(ctx)),
    );
}

fn read_meminfo() -> (u64, u64) {
    // (total, available) in bytes.
    let mut total = 0u64;
    let mut avail = 0u64;
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let key = it.next().unwrap_or("");
            let val: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            match key {
                "MemTotal:" => total = val * 1024,
                "MemAvailable:" => avail = val * 1024,
                _ => {}
            }
        }
    }
    (total, avail)
}

async fn cpu_percent() -> Option<f64> {
    // Two /proc/stat samples 200ms apart.
    let sample = || -> Option<(u64, u64)> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        let line = text.lines().next()?;
        let nums: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|v| v.parse().ok())
            .collect();
        if nums.len() < 4 {
            return None;
        }
        let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
        let total: u64 = nums.iter().sum();
        Some((idle, total))
    };
    let (i1, t1) = sample()?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let (i2, t2) = sample()?;
    let (di, dt) = (i2.saturating_sub(i1), t2.saturating_sub(t1));
    if dt == 0 {
        return None;
    }
    Some(100.0 * (1.0 - di as f64 / dt as f64))
}

fn net_io() -> (u64, u64) {
    // (bytes sent, bytes received) summed over non-loopback interfaces.
    let mut sent = 0u64;
    let mut recv = 0u64;
    if let Ok(text) = std::fs::read_to_string("/proc/net/dev") {
        for line in text.lines().skip(2) {
            let mut it = line.split_whitespace();
            let iface = it.next().unwrap_or("");
            if iface.starts_with("lo") {
                continue;
            }
            let vals: Vec<u64> = it.filter_map(|v| v.parse().ok()).collect();
            if vals.len() >= 9 {
                recv = recv.saturating_add(vals[0]);
                sent = sent.saturating_add(vals[8]);
            }
        }
    }
    (sent, recv)
}

async fn disk_usage() -> (u64, u64, u64, f64) {
    // (total, used, free, percent) for the current filesystem via `df`.
    let out = tokio::process::Command::new("df")
        .args(["-B1", "."])
        .output()
        .await;
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        if let Some(line) = text.lines().nth(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 5 {
                let total: u64 = cols[1].parse().unwrap_or(0);
                let used: u64 = cols[2].parse().unwrap_or(0);
                let free: u64 = cols[3].parse().unwrap_or(0);
                let pct = line
                    .split_whitespace()
                    .nth(4)
                    .and_then(|p| p.trim_end_matches('%').parse::<f64>().ok())
                    .unwrap_or(0.0);
                return (total, used, free, pct);
            }
        }
    }
    (0, 0, 0, 0.0)
}

async fn simple_usage() -> String {
    let (total, used, free, disk_pct) = disk_usage().await;
    let cpu = cpu_percent().await.unwrap_or(0.0);
    let (mem_total, mem_avail) = read_meminfo();
    let mem_pct = if mem_total > 0 {
        100.0 * (1.0 - mem_avail as f64 / mem_total as f64)
    } else {
        0.0
    };
    let (sent, recv) = net_io();
    format!(
        "**Disk**\nTotal: `{}`\nUsed: `{}`\nFree: `{}`\n\n**Network**\nUpload: `{}`\nDownload: `{}`\n\n**CPU**: `{cpu:.1}%`\n**Memory**: `{mem_pct:.1}%`\n**Disk**: `{disk_pct:.1}%`",
        humanbytes(total),
        humanbytes(used),
        humanbytes(free),
        humanbytes(sent),
        humanbytes(recv),
    )
}

async fn db_usage(ctx: &Ctx) -> Result<String> {
    // Real measurement: walk every stored key and sum key + value sizes.
    let keys = ctx.db.keys("").await?;
    let mut used: u64 = 0;
    for k in &keys {
        used += k.len() as u64;
        if let Some(v) = ctx.db.get(k).await? {
            used += v.len() as u64;
        }
    }
    Ok(format!(
        "**Database**\n\n**Keys**: `{}`\n**Storage Used**: `{}`",
        keys.len(),
        humanbytes(used)
    ))
}

async fn heroku_usage(ctx: &Ctx) -> Result<String> {
    let (Some(api), Some(app)) = (
        ctx.config.heroku_api.clone(),
        ctx.config.heroku_app_name.clone(),
    ) else {
        return Ok(
            "Please fill `HEROKU_API` and `HEROKU_APP_NAME` (or: this command is only for Heroku users)".to_string(),
        );
    };
    let account: serde_json::Value = ctx
        .http
        .get("https://api.heroku.com/account")
        .header("Authorization", format!("Bearer {api}"))
        .header("Accept", "application/vnd.heroku+json; version=3")
        .send()
        .await?
        .json()
        .await?;
    let user_id = account.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let quota: serde_json::Value = ctx
        .http
        .get(format!(
            "https://api.heroku.com/accounts/{user_id}/actions/get-quota"
        ))
        .header("Authorization", format!("Bearer {api}"))
        .header(
            "Accept",
            "application/vnd.heroku+json; version=3.account-quotas",
        )
        .send()
        .await?
        .json()
        .await?;
    let total = quota
        .get("account_quota")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let used = quota
        .get("quota_used")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let remaining = (total - used).max(0.0);
    let pct = if total > 0.0 {
        (remaining / total * 100.0).floor()
    } else {
        0.0
    };
    let mins_left = remaining / 60.0;
    let hours = (mins_left / 60.0).floor();
    let mins = (mins_left % 60.0).floor();
    let (dtotal, dused, dfree, disk_pct) = disk_usage().await;
    let cpu = cpu_percent().await.unwrap_or(0.0);
    let (mem_total, mem_avail) = read_meminfo();
    let mem_pct = if mem_total > 0 {
        100.0 * (1.0 - mem_avail as f64 / mem_total as f64)
    } else {
        0.0
    };
    let (sent, recv) = net_io();
    Ok(format!(
        "**Heroku — {app}**\n\n**Dyno quota left**: `{hours:.0}h {mins:.0}m` (`{pct:.0}%`)\n\n**Disk**\nTotal: `{}`\nUsed: `{}`\nFree: `{}`\n\n**Network**\nUpload: `{}`\nDownload: `{}`\n\n**CPU**: `{cpu:.1}%`\n**Memory**: `{mem_pct:.1}%`\n**Disk**: `{disk_pct:.1}%`",
        humanbytes(dtotal),
        humanbytes(dused),
        humanbytes(dfree),
        humanbytes(sent),
        humanbytes(recv),
    ))
}

async fn usage(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    let opt = ctx.text.split(' ').nth(1).unwrap_or("").trim();
    match opt {
        "db" => {
            let text = db_usage(ctx).await?;
            ctx.edit(id, &text).await?;
        }
        "heroku" => {
            let text = heroku_usage(ctx).await?;
            ctx.edit(id, &text).await?;
        }
        "" => {
            let text = simple_usage().await;
            ctx.edit(id, &text).await?;
        }
        _ => {
            let mut text = heroku_usage(ctx).await?;
            text.push_str("\n\n");
            text.push_str(&db_usage(ctx).await?);
            ctx.edit(id, &text).await?;
        }
    }
    Ok(())
}
