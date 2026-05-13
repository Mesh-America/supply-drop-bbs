//! System metrics collection using [`sysinfo`].
//!
//! [`collect`] is a blocking call (it sleeps briefly for a CPU sample).
//! Call it from `spawn_blocking` in async contexts.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{Disks, Networks, ProcessesToUpdate, System};

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Disk metrics for one mounted filesystem.
#[derive(Debug, Clone, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount: String,
    pub fs: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Network interface byte counters (cumulative since boot).
#[derive(Debug, Clone, Serialize)]
pub struct NetworkInfo {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// A single metrics snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    /// Global CPU usage percent (0–100).
    pub cpu_usage_pct: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    /// RSS of this process in bytes, if readable.
    pub process_rss_bytes: Option<u64>,
    pub disks: Vec<DiskInfo>,
    pub networks: Vec<NetworkInfo>,
    /// Unix timestamp of the sample.
    pub sampled_at: u64,
}

/// Collect a system metrics snapshot (blocking — sleeps ~200 ms for CPU delta).
pub fn collect() -> MetricsSnapshot {
    let mut sys = System::new();

    // Two-sample CPU measurement.
    sys.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    let cpu_usage_pct = sys.global_cpu_usage();

    // Memory.
    sys.refresh_memory();
    let memory_used_bytes = sys.used_memory();
    let memory_total_bytes = sys.total_memory();
    let swap_used_bytes = sys.used_swap();
    let swap_total_bytes = sys.total_swap();

    // Process RSS.
    let process_rss_bytes = sysinfo::get_current_pid().ok().and_then(|pid| {
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        sys.process(pid).map(|p| p.memory())
    });

    // Disks.
    let disks_raw = Disks::new_with_refreshed_list();
    let disks = disks_raw
        .iter()
        .map(|d| DiskInfo {
            name: d.name().to_string_lossy().into_owned(),
            mount: d.mount_point().to_string_lossy().into_owned(),
            fs: d.file_system().to_string_lossy().into_owned(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        })
        .collect();

    // Networks — skip loopback and zero-traffic interfaces.
    let networks = Networks::new_with_refreshed_list();
    let networks = networks
        .iter()
        .filter_map(|(name, data)| {
            let rx = data.total_received();
            let tx = data.total_transmitted();
            if rx == 0 && tx == 0 {
                return None;
            }
            Some(NetworkInfo {
                name: name.clone(),
                rx_bytes: rx,
                tx_bytes: tx,
            })
        })
        .collect();

    MetricsSnapshot {
        cpu_usage_pct,
        memory_used_bytes,
        memory_total_bytes,
        swap_used_bytes,
        swap_total_bytes,
        process_rss_bytes,
        disks,
        networks,
        sampled_at: unix_now(),
    }
}
