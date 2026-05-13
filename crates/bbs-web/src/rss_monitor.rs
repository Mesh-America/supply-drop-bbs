//! Rolling RSS (resident set size) trend monitor.
//!
//! Samples the current process's RSS every 30 seconds into a 60-sample
//! rolling window (~30 minutes of history). When [`ALERT_CONSECUTIVE`]
//! consecutive samples each show an increase AND the total growth over those
//! samples exceeds [`ALERT_MIN_GROWTH`], an [`RssAlert`] is broadcast.
//!
//! A second `cleared: true` alert fires when the RSS stops growing, so the
//! web UI can dismiss its badge automatically.
//!
//! Call [`start`] once from `WebPlugin::start`; keep the returned sender alive
//! in `AppState` so SSE subscribers can call `.subscribe()`.

use std::collections::VecDeque;

use serde::Serialize;
use sysinfo::{ProcessesToUpdate, System};
use tokio::sync::broadcast;

/// Samples in the rolling window (~30 min at 30 s interval).
const WINDOW: usize = 60;

/// Consecutive increasing samples required to fire an alert.
const ALERT_CONSECUTIVE: usize = 10;

/// Minimum total growth (bytes) over the alert run to suppress noise.
const ALERT_MIN_GROWTH: u64 = 5 * 1024 * 1024; // 5 MB

/// Time between samples.
const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// RSS growth alert payload sent over the broadcast channel.
#[derive(Debug, Clone, Serialize)]
pub struct RssAlert {
    /// Current RSS in bytes.
    pub rss_bytes: u64,
    /// Total growth over the consecutive-increase run.
    pub growth_bytes: u64,
    /// Length of the current consecutive-increase run.
    pub consecutive_increases: usize,
    /// `true` when the alert is being cleared (RSS stopped growing).
    pub cleared: bool,
}

/// Start the RSS monitor background task and return the broadcast sender.
///
/// The task runs until the process exits.  Keep the returned sender in shared
/// state so SSE handlers can call `.subscribe()`.
pub fn start() -> broadcast::Sender<RssAlert> {
    let (tx, _) = broadcast::channel(16);
    let tx2 = tx.clone();
    tokio::spawn(monitor_task(tx2));
    tx
}

async fn monitor_task(tx: broadcast::Sender<RssAlert>) {
    let pid = match sysinfo::get_current_pid() {
        Ok(p) => p,
        Err(_) => return, // platform does not support process PID lookup
    };

    let mut sys = System::new();
    let mut window: VecDeque<u64> = VecDeque::with_capacity(WINDOW + 1);
    let mut alert_active = false;

    loop {
        tokio::time::sleep(SAMPLE_INTERVAL).await;

        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        let rss = match sys.process(pid) {
            Some(p) => p.memory(),
            None => continue,
        };

        window.push_back(rss);
        if window.len() > WINDOW {
            window.pop_front();
        }

        if window.len() < 2 {
            continue;
        }

        let consecutive = consecutive_increases(&window);
        let growth = tail_growth(&window, consecutive);
        let firing = consecutive >= ALERT_CONSECUTIVE && growth >= ALERT_MIN_GROWTH;

        if firing && !alert_active {
            alert_active = true;
            let _ = tx.send(RssAlert {
                rss_bytes: rss,
                growth_bytes: growth,
                consecutive_increases: consecutive,
                cleared: false,
            });
        } else if !firing && alert_active && consecutive == 0 {
            alert_active = false;
            let _ = tx.send(RssAlert {
                rss_bytes: rss,
                growth_bytes: 0,
                consecutive_increases: 0,
                cleared: true,
            });
        }
    }
}

/// Count consecutive increasing samples at the tail of `window`.
fn consecutive_increases(window: &VecDeque<u64>) -> usize {
    let mut count = 0;
    let v: Vec<u64> = window.iter().copied().collect();
    for i in (1..v.len()).rev() {
        if v[i] > v[i - 1] {
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Total growth over the most recent `n` consecutive-increase samples.
fn tail_growth(window: &VecDeque<u64>, n: usize) -> u64 {
    if n == 0 || window.len() < 2 {
        return 0;
    }
    let tail_len = (n + 1).min(window.len());
    let start = window[window.len() - tail_len];
    let end = *window.back().unwrap_or(&0);
    end.saturating_sub(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_increases_returns_zero() {
        let w: VecDeque<u64> = [100, 90, 80].iter().copied().collect();
        assert_eq!(consecutive_increases(&w), 0);
    }

    #[test]
    fn all_increases_returns_n_minus_one() {
        let w: VecDeque<u64> = [10, 20, 30, 40].iter().copied().collect();
        assert_eq!(consecutive_increases(&w), 3);
    }

    #[test]
    fn partial_run_at_tail() {
        let w: VecDeque<u64> = [50, 40, 41, 42, 43].iter().copied().collect();
        assert_eq!(consecutive_increases(&w), 3);
    }

    #[test]
    fn tail_growth_zero_on_no_run() {
        let w: VecDeque<u64> = [100, 90, 80].iter().copied().collect();
        assert_eq!(tail_growth(&w, 0), 0);
    }

    #[test]
    fn tail_growth_calculates_correctly() {
        let w: VecDeque<u64> = [10, 20, 30, 40].iter().copied().collect();
        // 3 consecutive increases; start = w[0]=10, end = 40 → growth = 30
        assert_eq!(tail_growth(&w, 3), 30);
    }
}
