use super::model::{Aggregate, Minute, Values, SERIES};
use crate::system::config::persist_file;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

const MINUTE: u64 = 60_000;
const DAY: u64 = 1_440 * MINUTE;
const RETENTION: u64 = 7 * DAY;
const MAX_DAY_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Default)]
struct Bucket {
    weighted: [f64; SERIES.len()],
    peaks: Values,
    covered: [u32; SERIES.len()],
}
impl Bucket {
    fn add(&mut self, i: usize, average: f64, peak: f64, ms: u32) {
        let ms = ms.min(60_000 - self.covered[i]);
        if ms == 0 {
            return;
        }
        self.weighted[i] += average * f64::from(ms);
        self.peaks[i] = Some(self.peaks[i].unwrap_or(0.0).max(peak));
        self.covered[i] += ms;
    }
    fn minute(&self, time_ms: u64) -> Option<Minute> {
        let metrics = std::array::from_fn(|i| {
            let covered_ms = self.covered[i];
            (covered_ms > 0).then(|| Aggregate {
                average: self.weighted[i] / f64::from(covered_ms),
                peak: self.peaks[i].unwrap_or(0.0),
                covered_ms,
            })
        });
        metrics
            .iter()
            .any(Option::is_some)
            .then_some(Minute { time_ms, metrics })
    }
}

#[derive(Default)]
struct Reducer {
    previous: Option<(u64, Values)>,
    buckets: BTreeMap<u64, Bucket>,
}
impl Reducer {
    fn observe(&mut self, now: u64, elapsed: Option<u64>, values: Values) {
        if let Some((before, previous)) = self.previous.replace((now, values)) {
            let Some(span) = now
                .checked_sub(before)
                .filter(|span| (500..=20_000).contains(span))
            else {
                return;
            };
            if !elapsed.is_some_and(|elapsed| span.abs_diff(elapsed) <= 750) {
                return;
            }
            let mut cursor = before;
            while cursor < now {
                let minute = cursor / MINUTE * MINUTE;
                let end = now.min(minute + MINUTE);
                for i in 0..SERIES.len() {
                    // Memory is a point gauge; other counters describe the interval just ended.
                    if let Some(value) = if i < 3 { previous[i] } else { values[i] } {
                        self.buckets.entry(minute).or_default().add(
                            i,
                            value,
                            value,
                            (end - cursor) as u32,
                        );
                    }
                }
                cursor = end;
            }
        }
    }
}

pub struct History {
    dir: PathBuf,
    pub enabled: bool,
    pub error: Option<String>,
    reducer: Reducer,
    last_pruned_day: Option<u64>,
}
impl History {
    pub fn open(dir: PathBuf) -> Self {
        let (enabled, error) = match fs::read(dir.join("enabled")) {
            Ok(value) if value == b"1\n" => (true, None),
            Ok(value) if value == b"0\n" => (false, None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (false, None),
            _ => (false, Some("读取资源历史偏好失败；已保持关闭".into())),
        };
        Self {
            dir,
            enabled,
            error,
            reducer: Reducer::default(),
            last_pruned_day: None,
        }
    }
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        if enabled != self.enabled || self.error.is_some() {
            fs::create_dir_all(&self.dir)
                .and_then(|_| {
                    persist_file(
                        &self.dir.join("enabled"),
                        if enabled { b"1\n" } else { b"0\n" },
                    )
                })
                .map_err(|e| format!("无法保存资源历史偏好：{e}"))?;
            self.enabled = enabled;
            self.error = None;
            self.reducer.previous = None;
        }
        Ok(())
    }
    pub fn disconnect(&mut self) {
        self.reducer.previous = None;
    }
    pub fn flush(&mut self, now: u64) {
        self.enabled = false;
        self.disconnect();
        if let Some(row) = self
            .reducer
            .buckets
            .remove(&(now / MINUTE * MINUTE))
            .and_then(|b| b.minute(now / MINUTE * MINUTE))
        {
            let _ = self.append(&row);
        }
        self.maintain(now);
    }
    pub fn observe(&mut self, now: u64, elapsed: Option<u64>, values: Values) {
        if self.enabled {
            self.reducer.observe(now, elapsed, values);
        }
    }
    pub fn maintain(&mut self, now: u64) {
        // Keep failed writes bounded; there is no unbounded queue on a full volume.
        self.reducer
            .buckets
            .retain(|time, _| now.abs_diff(*time) <= RETENTION);
        let complete = now / MINUTE * MINUTE;
        let ready = self
            .reducer
            .buckets
            .range(..complete)
            .filter_map(|(time, bucket)| bucket.minute(*time))
            .collect::<Vec<_>>();
        let result = (|| -> io::Result<()> {
            for minute in ready {
                self.append(&minute)?;
                self.reducer.buckets.remove(&minute.time_ms);
            }
            let day = now / DAY;
            if self.last_pruned_day != Some(day) {
                self.prune(now)?;
                self.last_pruned_day = Some(day);
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(format!("资源历史写入或清理失败：{e}"));
        } else if self
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("资源历史写入"))
        {
            self.error = None;
        }
    }
    fn append(&self, minute: &Minute) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("day-{}.jsonl", minute.time_ms / DAY));
        regular_file(&path)?;
        let json = serde_json::to_string(minute)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata()?.len();
        if len + json.len() as u64 + 2 > MAX_DAY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resource history day limit",
            ));
        }
        // If a previous run died mid-write, do not concatenate the next row to it.
        file.write_all(format!("\n{json}\n").as_bytes())?;
        file.sync_data()
    }
    fn day_paths(&self) -> io::Result<Vec<(u64, PathBuf)>> {
        let dir = match fs::read_dir(&self.dir) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut result = Vec::new();
        for entry in dir {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(day) = owned_day(&entry.file_name().to_string_lossy()) {
                result.push((day, entry.path()));
            }
        }
        Ok(result)
    }
    fn prune(&self, now: u64) -> io::Result<()> {
        let earliest = now.saturating_sub(RETENTION) / DAY;
        for (day, path) in self.day_paths()? {
            if day < earliest {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
    pub fn clear(&mut self) -> Result<(), String> {
        self.reducer = Reducer::default();
        let result = (|| -> io::Result<()> {
            for (_, path) in self.day_paths()? {
                fs::remove_file(path)?;
            }
            Ok(())
        })()
        .map_err(|e| format!("部分资源历史无法清除：{e}"));
        if let Err(error) = &result {
            self.error = Some(error.clone());
        } else {
            self.error = None;
        }
        result
    }
    pub fn read(&self, now: u64, hours: u16) -> Result<Vec<Minute>, String> {
        if !matches!(hours, 1 | 24 | 168) {
            return Err("请输入 1、24 或 168 小时".into());
        }
        let from = now
            .saturating_sub(u64::from(hours) * 60 * MINUTE)
            .div_ceil(MINUTE)
            * MINUTE;
        let mut result = BTreeMap::<u64, Bucket>::new();
        let mut insert = |row: Minute, day: u64| {
            if row.time_ms / DAY != day
                || row.time_ms % MINUTE != 0
                || !(from..=now).contains(&row.time_ms)
            {
                return;
            }
            let bucket = result.entry(row.time_ms).or_default();
            for (i, value) in row.metrics.iter().enumerate() {
                if let Some(value) = value.filter(|v| {
                    (1..=60_000).contains(&v.covered_ms)
                        && v.average.is_finite()
                        && v.peak.is_finite()
                        && v.average >= 0.0
                        && v.peak >= v.average * (1.0 - 1e-12)
                }) {
                    let ms = value.covered_ms.min(60_000 - bucket.covered[i]);
                    if ms != 0 {
                        bucket.add(i, value.average, value.peak, ms);
                    }
                }
            }
        };
        let days = self
            .day_paths()
            .map_err(|e| format!("无法读取资源历史：{e}"))?;
        for (day, path) in days {
            if !(from / DAY..=now / DAY).contains(&day) {
                continue;
            }
            let file = fs::File::open(path).map_err(|e| format!("无法打开资源历史：{e}"))?;
            if file.metadata().map_err(|e| e.to_string())?.len() > MAX_DAY_BYTES {
                return Err("资源历史文件过大".into());
            }
            for line in BufReader::new(file).split(b'\n') {
                let line = line.map_err(|e| format!("无法读取资源历史：{e}"))?;
                if let Ok(row) = serde_json::from_slice::<Minute>(&line) {
                    insert(row, day);
                }
            }
        }
        for (time, bucket) in self.reducer.buckets.range(from..=now) {
            if let Some(row) = bucket.minute(*time) {
                insert(row, *time / DAY);
            }
        }
        Ok(result
            .into_iter()
            .filter_map(|(time, bucket)| bucket.minute(time))
            .collect())
    }
}

fn owned_day(name: &str) -> Option<u64> {
    let day = name.strip_prefix("day-")?.strip_suffix(".jsonl")?;
    let n: u64 = day.parse().ok()?;
    (day == n.to_string()).then_some(n)
}

fn regular_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular history file",
        )),
    }
}

pub fn csv(rows: &[Minute]) -> Result<String, String> {
    use std::fmt::Write;
    if rows.is_empty() {
        return Err("这个时段没有可导出的资源汇总".into());
    }
    let mut csv = String::from("minute_start_unix_ms_utc");
    for key in SERIES {
        let _ = write!(csv, ",{key}_avg,{key}_peak,{key}_covered_ms");
    }
    csv.push_str("\r\n");
    for row in rows {
        let _ = write!(csv, "{}", row.time_ms);
        for value in row.metrics {
            if let Some(a) = value {
                let _ = write!(csv, ",{:.3},{:.3},{}", a.average, a.peak, a.covered_ms);
            } else {
                csv.push_str(",,,0");
            }
        }
        csv.push_str("\r\n");
    }
    Ok(csv)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_minutes_preserve_coverage_gaps_and_split_utc_boundaries() {
        let mut reducer = Reducer::default();
        let mut first = [None; SERIES.len()];
        first[0] = Some(10.0);
        let mut next = first;
        next[0] = Some(20.0);
        next[3] = Some(80.0);
        reducer.observe(58_000, None, first);
        reducer.observe(62_000, Some(4_000), next);
        reducer.observe(90_000, Some(28_000), next);
        reducer.observe(99_000, Some(2_000), next);
        reducer.observe(101_000, Some(2_000), next);
        let m0 = reducer.buckets[&0].minute(0).unwrap();
        let m1 = reducer.buckets[&60_000].minute(60_000).unwrap();
        assert_eq!(m0.metrics[0].unwrap().covered_ms, 2_000);
        assert_eq!(m1.metrics[0].unwrap().average, 15.0);
        assert_eq!(m1.metrics[0].unwrap().peak, 20.0);
        assert_eq!(m1.metrics[3].unwrap().covered_ms, 4_000);
        assert!(m1.metrics[4].is_none());
    }
    #[test]
    fn resource_history_opt_in_restart_retention_export_and_clear() {
        let dir = std::env::temp_dir().join(format!(
            "openrevo-resource-history-test-{}",
            std::process::id()
        ));
        let mut history = History::open(dir.clone());
        assert!(!history.enabled);
        let now = 30 * DAY;
        let mut values = [None; SERIES.len()];
        values[0] = Some(100.0);
        history.observe(now, None, values);
        assert!(!dir.exists());
        history.set_enabled(true).unwrap();
        history.observe(now, None, values);
        history.observe(now + 10_000, Some(10_000), values);
        history.disconnect();
        history.observe(now + 18_000, None, values);
        history.observe(now + 20_000, Some(2_000), values);
        history.maintain(now + MINUTE);
        fs::write(dir.join("day-1.jsonl"), b"garbage").unwrap();
        fs::write(dir.join("unrelated.txt"), b"keep").unwrap();
        let mut restarted = History::open(dir.clone());
        assert!(restarted.enabled);
        restarted.maintain(now + MINUTE);
        assert!(!dir.join("day-1.jsonl").exists());
        let rows = restarted.read(now + MINUTE, 1).unwrap();
        assert_eq!(rows[0].metrics[0].unwrap().covered_ms, 12_000);
        assert!(csv(&rows).unwrap().contains("100.000,100.000,12000"));
        restarted.set_enabled(false).unwrap();
        assert_eq!(
            History::open(dir.clone())
                .read(now + MINUTE, 1)
                .unwrap()
                .len(),
            1
        );
        restarted.clear().unwrap();
        assert!(restarted.read(now + MINUTE, 168).unwrap().is_empty());
        for name in ["enabled", "unrelated.txt"] {
            fs::remove_file(dir.join(name)).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }
}
