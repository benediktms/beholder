use std::{
    fmt, io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
};

// ponytail: native ps keeps this dependency-free; use kernel accounting if 100ms overshoot matters.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub(crate) enum MemoryGuardEvent {
    Exceeded { limit_bytes: u64, peak_bytes: u64 },
    Failed(String),
}

impl fmt::Display for MemoryGuardEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exceeded {
                limit_bytes,
                peak_bytes,
            } => write!(
                formatter,
                "worker process tree exceeded its {limit_bytes}-byte memory limit (sampled {peak_bytes} bytes)"
            ),
            Self::Failed(error) => write!(formatter, "worker memory guard failed: {error}"),
        }
    }
}

pub(crate) struct ProcessMemoryGuard {
    process_group: i32,
    peak_bytes: Arc<AtomicU64>,
    events: watch::Receiver<Option<MemoryGuardEvent>>,
    task: JoinHandle<()>,
}

impl ProcessMemoryGuard {
    pub(crate) async fn start(process_group: i32, limit_bytes: u64) -> io::Result<Self> {
        let initial = process_group_rss_bytes(process_group).await?;
        let peak_bytes = Arc::new(AtomicU64::new(initial));
        let initial_event = (initial > limit_bytes).then_some(MemoryGuardEvent::Exceeded {
            limit_bytes,
            peak_bytes: initial,
        });
        let (events, receiver) = watch::channel(initial_event);
        let sampled_peak = Arc::clone(&peak_bytes);
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            loop {
                interval.tick().await;
                match process_group_rss_bytes(process_group).await {
                    Ok(current) => {
                        sampled_peak.fetch_max(current, Ordering::Relaxed);
                        if current > limit_bytes {
                            let _ = events.send(Some(MemoryGuardEvent::Exceeded {
                                limit_bytes,
                                peak_bytes: current,
                            }));
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = events.send(Some(MemoryGuardEvent::Failed(error.to_string())));
                        return;
                    }
                }
            }
        });
        Ok(Self {
            process_group,
            peak_bytes,
            events: receiver,
            task,
        })
    }

    pub(crate) async fn event(&mut self) -> MemoryGuardEvent {
        loop {
            if let Some(event) = self.events.borrow().clone() {
                return event;
            }
            if self.events.changed().await.is_err() {
                return MemoryGuardEvent::Failed("memory sampler stopped unexpectedly".into());
            }
        }
    }

    pub(crate) fn peak_bytes(&self) -> u64 {
        self.peak_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn process_group(&self) -> i32 {
        self.process_group
    }
}

impl Drop for ProcessMemoryGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) fn isolate_process_group(command: &mut Command) {
    command.process_group(0);
}

pub(crate) async fn terminate_process_group(
    process_group: i32,
    child: &mut Child,
) -> io::Result<()> {
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    child.wait().await.map(|_| ())
}

async fn process_group_rss_bytes(process_group: i32) -> io::Result<u64> {
    let output = Command::new("ps")
        .args(["-axo", "pgid=,rss="])
        .output()
        .await?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ps exited with {}",
            output.status
        )));
    }
    parse_process_group_rss_bytes(&String::from_utf8_lossy(&output.stdout), process_group)
}

fn parse_process_group_rss_bytes(output: &str, process_group: i32) -> io::Result<u64> {
    output.lines().try_fold(0_u64, |total, line| {
        let mut fields = line.split_whitespace();
        let group = fields
            .next()
            .ok_or_else(|| io::Error::other("ps omitted process group"))?
            .parse::<i32>()
            .map_err(io::Error::other)?;
        let rss_kib = fields
            .next()
            .ok_or_else(|| io::Error::other("ps omitted resident memory"))?
            .parse::<u64>()
            .map_err(io::Error::other)?;
        if fields.next().is_some() {
            return Err(io::Error::other("ps returned unexpected fields"));
        }
        if group == process_group {
            total
                .checked_add(rss_kib.saturating_mul(1024))
                .ok_or_else(|| io::Error::other("process memory total overflowed"))
        } else {
            Ok(total)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::SystemTime};

    #[test]
    fn parses_aggregate_process_group_rss() {
        assert_eq!(
            parse_process_group_rss_bytes("  42 100\n  7 999\n 42 25\n", 42).unwrap(),
            125 * 1024
        );
    }

    #[tokio::test]
    async fn memory_limit_terminates_the_worker_process_group() {
        let root = std::env::temp_dir().join(format!(
            "beholder-memory-guard-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let child_pid = root.join("child.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 30 & echo $! > \"$CHILD_PID\"; wait")
            .env("CHILD_PID", &child_pid);
        isolate_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let process_group = child.id().unwrap() as i32;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !child_pid.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut guard = ProcessMemoryGuard::start(process_group, 1).await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), guard.event())
            .await
            .unwrap();
        terminate_process_group(process_group, &mut child)
            .await
            .unwrap();

        assert!(matches!(event, MemoryGuardEvent::Exceeded { .. }));
        tokio::time::timeout(Duration::from_secs(2), async {
            while process_group_rss_bytes(process_group).await.unwrap() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
