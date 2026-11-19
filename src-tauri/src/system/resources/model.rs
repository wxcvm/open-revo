use serde::{Deserialize, Serialize};

pub const SERIES: [&str; 10] = [
    "ram_used_bytes",
    "ram_available_bytes",
    "ram_commit_bytes",
    "cpu_percent",
    "disk_read_bps",
    "disk_write_bps",
    "disk_read_iops",
    "disk_write_iops",
    "network_rx_bps",
    "network_tx_bps",
];
pub type Values = [Option<f64>; SERIES.len()];

#[derive(Clone, Default, Serialize)]
pub struct Memory {
    pub installed: Option<u64>,
    pub visible: Option<u64>,
    pub reserved: Option<u64>,
    pub available: Option<u64>,
    pub used: Option<u64>,
    pub standby: Option<u64>,
    pub free_zero: Option<u64>,
    pub available_unclassified: Option<u64>,
    pub available_split_error: Option<&'static str>,
    pub modified: Option<u64>,
    pub nonpaged_pool: Option<u64>,
    pub paged_pool_resident: Option<u64>,
    pub cache_resident: Option<u64>,
    pub commit: Option<u64>,
    pub commit_limit: Option<u64>,
    pub private_resident_sum: Option<u64>,
    pub unattributed_used: Option<u64>,
    pub private_readable: usize,
    pub private_seen: usize,
}

impl Memory {
    pub fn reconcile_available(&mut self, standby: Option<u64>, free_zero: Option<u64>) {
        let result = self
            .available
            .zip(standby)
            .zip(free_zero)
            .and_then(|((available, standby), free)| {
                Some((
                    standby,
                    free,
                    available.checked_sub(standby.checked_add(free)?)?,
                ))
            })
            .filter(|(_, _, delta)| *delta <= 64 * 1024 * 1024);
        if let Some((standby, free, remainder)) = result {
            self.standby = Some(standby);
            self.free_zero = Some(free);
            self.available_unclassified = Some(remainder);
            self.available_split_error = None;
        } else {
            self.standby = None;
            self.free_zero = None;
            self.available_unclassified = None;
            self.available_split_error = Some("待机与空闲页计数缺失或无法与可用量对账");
        }
    }

    pub fn reconcile_processes(&mut self, rows: &[Process]) {
        self.private_seen = rows.len();
        self.private_readable = rows.iter().filter(|r| r.private_resident.is_some()).count();
        if self.private_readable > 0 {
            self.private_resident_sum = rows
                .iter()
                .filter_map(|r| r.private_resident)
                .try_fold(0u64, u64::checked_add);
            self.unattributed_used = self
                .used
                .zip(self.private_resident_sum)
                .and_then(|(used, private)| used.checked_sub(private));
        }
    }
}

#[derive(Clone, Serialize)]
pub struct Process {
    // A missing creation stamp is sufficient for a current row, not a trend identity.
    pub key: Option<String>,
    pub pid: u32,
    pub name: String,
    pub private_resident: Option<u64>,
    pub private_commit: Option<u64>,
    pub cpu_percent: Option<f64>,
}

#[derive(Clone, Default, Serialize)]
pub struct Disk {
    pub key: String,
    pub name: String,
    pub read: Option<f64>,
    pub write: Option<f64>,
    pub read_iops: Option<f64>,
    pub write_iops: Option<f64>,
}

#[derive(Clone, Serialize)]
pub struct Interface {
    pub key: String,
    pub name: String,
    pub physical: bool,
    pub up: bool,
    pub rx: Option<f64>,
    pub tx: Option<f64>,
}

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub time_ms: u64,
    pub sampled_in_ms: u64,
    pub summary: Values,
    pub memory: Memory,
    pub processes: Vec<Process>,
    pub disks: Vec<Disk>,
    pub interfaces: Vec<Interface>,
    pub process_source: Option<&'static str>,
    pub disk_detail_available: bool,
    pub net_available: bool,
    pub logical_cpus: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Aggregate {
    pub average: f64,
    pub peak: f64,
    pub covered_ms: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Minute {
    pub time_ms: u64,
    pub metrics: [Option<Aggregate>; SERIES.len()],
}

/// A counter reset, disconnect or warmup is not zero traffic.
pub fn rate(previous: u64, current: u64, seconds: f64) -> Option<f64> {
    (0.5..=20.0)
        .contains(&seconds)
        .then(|| {
            current
                .checked_sub(previous)
                .map(|delta| delta as f64 / seconds)
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_memory_checks_partitions_and_unreadable_processes() {
        let unit = 1024 * 1024;
        let mut m = Memory {
            available: Some(100 * unit),
            used: Some(500 * unit),
            ..Default::default()
        };
        m.reconcile_available(Some(80 * unit), Some(20 * unit));
        assert_eq!(m.available_unclassified, Some(0));
        m.reconcile_available(Some(110 * unit), Some(20 * unit));
        assert!(m.standby.is_none() && m.free_zero.is_none());
        let proc = |value, pid| Process {
            key: None,
            pid,
            name: String::new(),
            private_resident: value,
            private_commit: None,
            cpu_percent: None,
        };
        m.reconcile_processes(&[proc(Some(300 * unit), 1), proc(None, 2)]);
        assert_eq!(m.unattributed_used, Some(200 * unit));
        assert_eq!((m.private_readable, m.private_seen), (1, 2));
        m.reconcile_processes(&[proc(Some(550 * unit), 3)]);
        assert_eq!(m.unattributed_used, None);
    }
    #[test]
    fn resource_rates_do_not_turn_reset_or_sleep_into_zero() {
        assert_eq!(rate(10, 10, 2.0), Some(0.0));
        assert_eq!(rate(10, 14, 2.0), Some(2.0));
        assert_eq!(rate(10, 9, 2.0), None);
        assert_eq!(rate(10, 300, 50.0), None);
    }
}
