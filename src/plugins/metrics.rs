//! helpers misplaced in the supervisor (MA-11): child log draining and
//! /proc resource reads

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::Mutex;

/// Drain a child stream into the plugin's ring log buffer.
pub(crate) fn drain_to_log<S>(
    stream: S,
    buf: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
    mirror_to_kernel_log: bool,
) where
    S: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if mirror_to_kernel_log {
                eprintln!("[plugin-stderr] {line}");
            }
            let mut locked = buf.lock().await;
            if locked.len() >= max_lines {
                locked.pop_front();
            }
            locked.push_back(line);
        }
    });
}

/// Read CPU seconds (user+system) and RSS bytes for a given PID from `/proc`.
/// Returns `(cpu_seconds, rss_bytes)` or None on any read/parse failure.
#[cfg(target_os = "linux")]
pub(crate) fn proc_resource_usage(pid: u32) -> Option<(f64, f64)> {
    // --- CPU time from /proc/<pid>/stat ---
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.split_whitespace().collect();
    // utime = field 14 (0-indexed: 13), stime = field 15 (0-indexed: 14)
    let utime: u64 = fields.get(13)?.parse().ok()?;
    let stime: u64 = fields.get(14)?.parse().ok()?;
    let clk_tck = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        .ok()
        .flatten()
        .unwrap_or(100) as f64;
    let cpu_seconds = (utime + stime) as f64 / clk_tck;

    // --- RSS from /proc/<pid>/status ---
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb: f64 = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let rss_bytes = rss_kb * 1024.0;

    Some((cpu_seconds, rss_bytes))
}
