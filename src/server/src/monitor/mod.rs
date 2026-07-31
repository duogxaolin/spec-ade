//! Host metrics + process control.
//!
//! Spec: `docs/specs/SPEC-006-search-monitor.md` §3.2–§3.4, §5.5–§5.7.
//!
//! Three rules from `sysinfo` 0.39 shape this module (04 §7):
//!
//! 1. **One long-lived `System`.** CPU usage is a *delta* between two refreshes,
//!    so a handler that builds a fresh `System` per request always reports 0.0%.
//!    A single sampler owns the instance and every reader gets its latest sample
//!    (§5.5).
//! 2. **Two refreshes at least [`MINIMUM_CPU_UPDATE_INTERVAL`] apart.** The first
//!    sample is therefore discarded, never broadcast.
//! 3. **`System::new()`, not `new_all()`.** `new_all` enumerates every process,
//!    every disk and every component up front; we refresh only CPU, memory and
//!    processes, on a timer.
//!
//! The sampler is subscriber-driven like the git watcher (SPEC-005 §5.6): it runs
//! only while something is listening, so an idle server does not enumerate the
//! process table forever ([SPEC-006 INVENTED-12]).

pub mod gpu;

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{
    MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, Users,
};
use tokio::sync::{Mutex, broadcast};

/// Interval between samples ([SPEC-006 INVENTED-9]).
///
/// 3s is the cadence the API contract documents for the metrics panel; faster
/// would cost a full process-table walk for a sparkline nobody watches that
/// closely.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);

/// Default number of processes returned.
pub const DEFAULT_TOP_N: usize = 30;

/// Ceiling on `topN`, whatever the client asks for. A 2000-process listing is
/// megabytes of JSON no UI renders.
pub const MAX_TOP_N: usize = 200;

/// Broadcast depth. Small: the payload is state, so only the newest matters.
const CHANNEL_CAPACITY: usize = 4;

/// How long the sampler keeps running after its last subscriber leaves.
///
/// Zero would restart the whole warm-up (including the discarded first sample)
/// every time a browser tab reconnects; one interval of grace makes a reload
/// cheap without keeping the sampler alive on an idle server.
const IDLE_GRACE: Duration = Duration::from_secs(10);

// ---- DTOs ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    /// Wall-clock of the sample, so the client can tell a stale frame from a
    /// fresh one and space its sparkline correctly.
    pub timestamp_ms: u64,
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub host: HostMetrics,
    /// `null` when no GPU is available — the UI hides the section rather than
    /// showing an error (§1).
    pub gpu: Option<gpu::GpuMetrics>,
    pub processes: Vec<ProcessInfo>,
    /// Total number of processes on the host, **not** `processes.len()`. Without
    /// it a top-30 listing would silently claim the machine has 30 processes.
    pub process_count: usize,
    /// `process_count > processes.len()`.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuMetrics {
    /// Global usage, 0–100.
    pub usage: f32,
    pub core_count: usize,
    pub per_core: Vec<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryMetrics {
    /// Bytes.
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostMetrics {
    pub name: Option<String>,
    pub os: Option<String>,
    pub uptime_sec: u64,
    /// 1/5/15-minute load average. All zeroes on Windows, where the concept does
    /// not exist.
    pub load_avg: [f64; 3],
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    /// Full command line, joined. Empty for kernel threads.
    pub cmd: String,
    /// Percent of one core — can exceed 100 on a multithreaded process, which is
    /// what `top` shows too.
    pub cpu: f32,
    /// Resident memory in bytes.
    pub memory: u64,
    pub status: String,
    pub run_time_sec: u64,
    pub user: Option<String>,
}

/// How the process list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Cpu,
    Memory,
}

impl SortBy {
    /// Parse the `?sort=` param. Anything unrecognized falls back to CPU rather
    /// than 400-ing: a bad sort key is not worth failing a read-only request.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("memory") | Some("mem") => SortBy::Memory,
            _ => SortBy::Cpu,
        }
    }
}

// ---- sampler ---------------------------------------------------------------

/// The shared sampler: one `System`, one task, many readers.
#[derive(Clone, Default)]
pub struct MetricsSampler {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    tx: Option<broadcast::Sender<Arc<Metrics>>>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Newest sample, so `GET /metrics` answers immediately instead of paying
    /// the two-refresh warm-up per request (§5.5).
    latest: Option<Arc<Metrics>>,
}

impl std::fmt::Debug for MetricsSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MetricsSampler")
    }
}

impl MetricsSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to the metrics stream, starting the sampler if it is not running.
    pub async fn subscribe(&self) -> broadcast::Receiver<Arc<Metrics>> {
        let mut inner = self.inner.lock().await;
        self.ensure_running(&mut inner);
        inner
            .tx
            .as_ref()
            .expect("ensure_running installs a sender")
            .subscribe()
    }

    /// The newest sample, sampling once inline if the sampler has not produced
    /// one yet.
    ///
    /// The inline path pays the [`MINIMUM_CPU_UPDATE_INTERVAL`] warm-up exactly
    /// once — the alternative, returning `cpu.usage = 0.0` on the first request,
    /// is a wrong number rather than a slow one (D23).
    pub async fn latest(&self) -> Arc<Metrics> {
        {
            let mut inner = self.inner.lock().await;
            self.ensure_running(&mut inner);
            if let Some(latest) = &inner.latest {
                return Arc::clone(latest);
            }
        }

        // Wait for the sampler's first real sample rather than building a second
        // `System` here: two instances would double the process enumeration and
        // still disagree about CPU deltas.
        let mut rx = self.subscribe().await;
        match tokio::time::timeout(MINIMUM_CPU_UPDATE_INTERVAL * 4 + SAMPLE_INTERVAL, rx.recv())
            .await
        {
            Ok(Ok(metrics)) => metrics,
            // The sampler died or is wedged; a synchronous sample is worse than
            // nothing only if it lies, and a fresh two-refresh read does not.
            _ => Arc::new(sample_twice().await),
        }
    }

    /// Stop the sampler. Used on shutdown and in tests.
    pub async fn stop(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.task.take() {
            task.abort();
        }
        inner.tx = None;
        inner.latest = None;
    }

    /// Whether a sampler task is currently alive. Public because "one sampler for
    /// two subscribers" (D29) and "the sampler stops" (D30) are claims about
    /// lifecycle that nothing else can observe.
    pub async fn is_running(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.task.as_ref().is_some_and(|t| !t.is_finished())
    }

    fn ensure_running(&self, inner: &mut Inner) {
        if inner.task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        let shared = Arc::clone(&self.inner);
        let task = tokio::spawn(sample_loop(tx.clone(), shared));
        inner.tx = Some(tx);
        inner.task = Some(task);
        // A restarted sampler has no valid sample yet; keeping the old one would
        // hand out a snapshot from an arbitrary time in the past.
        inner.latest = None;
    }
}

/// The sampler task.
///
/// Shape is dictated by rule 2 above: refresh, wait out the minimum interval,
/// refresh again, and only then start broadcasting. The first refresh pair is the
/// warm-up; its CPU numbers are meaningless and are never sent.
async fn sample_loop(tx: broadcast::Sender<Arc<Metrics>>, shared: Arc<Mutex<Inner>>) {
    let mut system = System::new();
    let mut users = Users::new_with_refreshed_list();

    refresh(&mut system);
    tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;

    let mut idle_since: Option<std::time::Instant> = None;

    loop {
        refresh(&mut system);
        let metrics = Arc::new(build(&system, &users));

        {
            let mut inner = shared.lock().await;
            inner.latest = Some(Arc::clone(&metrics));
        }
        let _ = tx.send(metrics);

        // Users change rarely; re-reading the list every sample would be a file
        // read per tick for data that is effectively static.
        if idle_since.is_none() {
            users.refresh();
        }

        tokio::time::sleep(SAMPLE_INTERVAL).await;

        // Subscriber-driven shutdown, with grace so a page reload does not force
        // a fresh warm-up ([SPEC-006 INVENTED-12]).
        if tx.receiver_count() == 0 {
            match idle_since {
                Some(since) if since.elapsed() >= IDLE_GRACE => {
                    let mut inner = shared.lock().await;
                    inner.latest = None;
                    return;
                }
                Some(_) => {}
                None => idle_since = Some(std::time::Instant::now()),
            }
        } else {
            idle_since = None;
        }
    }
}

/// Selective refresh — CPU, memory, processes. Nothing else is in the DTO, and
/// each extra `refresh_*` is real syscalls per tick.
fn refresh(system: &mut System) {
    system.refresh_cpu_all();
    system.refresh_memory();
    system.refresh_processes(ProcessesToUpdate::All, true);
}

/// Build a snapshot with a full two-refresh warm-up.
///
/// Only used as the fallback in [`MetricsSampler::latest`]; the sampler itself
/// keeps its `System` alive across ticks.
async fn sample_twice() -> Metrics {
    let users = Users::new_with_refreshed_list();
    let mut system = System::new();
    refresh(&mut system);
    tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
    refresh(&mut system);
    build(&system, &users)
}

fn build(system: &System, users: &Users) -> Metrics {
    let load = System::load_average();

    let mut processes: Vec<ProcessInfo> = system
        .processes()
        .values()
        .map(|p| ProcessInfo {
            pid: p.pid().as_u32(),
            parent_pid: p.parent().map(|p| p.as_u32()),
            name: p.name().to_string_lossy().into_owned(),
            cmd: p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            cpu: p.cpu_usage(),
            memory: p.memory(),
            status: p.status().to_string(),
            run_time_sec: p.run_time(),
            user: p
                .user_id()
                .and_then(|uid| users.get_user_by_id(uid))
                .map(|u| u.name().to_string()),
        })
        .collect();

    let process_count = processes.len();
    // Sorted here, capped by the handler: the sampler has no idea what `topN` the
    // caller wants, and sorting once beats sorting per subscriber.
    sort_processes(&mut processes, SortBy::Cpu);

    Metrics {
        timestamp_ms: unix_millis(),
        cpu: CpuMetrics {
            usage: system.global_cpu_usage(),
            core_count: system.cpus().len(),
            per_core: system.cpus().iter().map(|c| c.cpu_usage()).collect(),
        },
        memory: MemoryMetrics {
            total: system.total_memory(),
            used: system.used_memory(),
            swap_total: system.total_swap(),
            swap_used: system.used_swap(),
        },
        host: HostMetrics {
            name: System::host_name(),
            os: System::long_os_version().or_else(System::name),
            uptime_sec: System::uptime(),
            load_avg: [load.one, load.five, load.fifteen],
        },
        gpu: gpu::sample(),
        processes,
        process_count,
        truncated: false,
    }
}

/// Order by the requested key, descending, with the name as a tiebreak.
///
/// The tiebreak matters more than it looks: without it, dozens of idle processes
/// at exactly 0.0% CPU reshuffle on every 3s sample and the list flickers.
pub fn sort_processes(processes: &mut [ProcessInfo], sort: SortBy) {
    match sort {
        SortBy::Cpu => processes.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.pid.cmp(&b.pid))
        }),
        SortBy::Memory => processes.sort_by(|a, b| {
            b.memory
                .cmp(&a.memory)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.pid.cmp(&b.pid))
        }),
    }
}

/// Apply `sort` and `topN` to a snapshot, keeping `processCount` truthful.
pub fn narrow(metrics: &Metrics, sort: SortBy, top_n: usize) -> Metrics {
    let top_n = top_n.clamp(1, MAX_TOP_N);
    let mut out = metrics.clone();
    sort_processes(&mut out.processes, sort);
    out.truncated = out.processes.len() > top_n;
    out.processes.truncate(top_n);
    out
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---- kill ------------------------------------------------------------------

/// Which signal `POST /api/system/kill/{pid}` sends ([SPEC-006 INVENTED-10]).
///
/// `term` is the default, **not** `kill`: `sysinfo`'s `Process::kill()` is
/// `kill_with(Signal::Kill)`, which gives a dev server no chance to flush or clean
/// up its child processes. An IDE should ask before it forces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    Term,
    Kill,
    Int,
    Hup,
}

impl KillSignal {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "term" | "sigterm" | "" => Some(KillSignal::Term),
            "kill" | "sigkill" => Some(KillSignal::Kill),
            "int" | "sigint" => Some(KillSignal::Int),
            "hup" | "sighup" => Some(KillSignal::Hup),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            KillSignal::Term => "term",
            KillSignal::Kill => "kill",
            KillSignal::Int => "int",
            KillSignal::Hup => "hup",
        }
    }

    fn to_sysinfo(self) -> Signal {
        match self {
            KillSignal::Term => Signal::Term,
            KillSignal::Kill => Signal::Kill,
            KillSignal::Int => Signal::Interrupt,
            KillSignal::Hup => Signal::Hangup,
        }
    }
}

/// Why a kill did not happen.
#[derive(Debug, thiserror::Error)]
pub enum KillError {
    #[error("unknown signal: {0}")]
    BadSignal(String),
    #[error("no such process: {0}")]
    NotFound(u32),
    /// Refusing to kill our own server, or pid 0/1 ([SPEC-006 INVENTED-11]).
    #[error("{0}")]
    Refused(String),
    /// `kill_with` returned `None`: the platform has no such signal. Distinct
    /// from a failed send, which is a permission problem (§3.4).
    #[error("signal {0} is not supported on this platform")]
    Unsupported(&'static str),
    #[error("failed to signal process {0}")]
    Failed(u32),
}

/// Send `signal` to `pid`.
///
/// Blocking (`refresh_processes` walks `/proc`), so the caller wraps it in
/// `spawn_blocking`.
///
/// The refresh immediately before the lookup is load-bearing (§9 #11): pids are
/// reused, and signalling a stale snapshot's entry means signalling whatever now
/// holds that number.
pub fn kill(pid: u32, signal: KillSignal) -> Result<(), KillError> {
    let own = std::process::id();
    if pid == own {
        return Err(KillError::Refused(
            "refusing to kill the Spec ADE server itself".into(),
        ));
    }
    // pid 0 is "every process in the group" on POSIX and pid 1 is init: neither is
    // something a click in a process list should be able to reach.
    if pid == 0 || pid == 1 {
        return Err(KillError::Refused(format!("refusing to signal pid {pid}")));
    }

    let target = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target]),
        true,
        ProcessRefreshKind::nothing(),
    );

    let process = system.process(target).ok_or(KillError::NotFound(pid))?;
    match process.kill_with(signal.to_sysinfo()) {
        // `None` means the signal does not exist here (Windows has no SIGHUP);
        // `Some(false)` means it exists and the send was refused — usually EPERM.
        None => Err(KillError::Unsupported(signal.name())),
        Some(false) => Err(KillError::Failed(pid)),
        Some(true) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_at(pid: u32, name: &str, cpu: f32, memory: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            cmd: String::new(),
            cpu,
            memory,
            status: "Run".into(),
            run_time_sec: 0,
            user: None,
        }
    }

    #[test]
    fn sorts_by_cpu_descending_with_a_stable_tiebreak() {
        let mut list = vec![
            proc_at(3, "c", 0.0, 10),
            proc_at(1, "a", 50.0, 10),
            proc_at(2, "b", 0.0, 10),
        ];
        sort_processes(&mut list, SortBy::Cpu);
        assert_eq!(
            list.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        // The tiebreak is what stops a list of idle 0.0% processes from
        // reshuffling on every 3s sample.
        let mut shuffled = vec![
            proc_at(2, "b", 0.0, 10),
            proc_at(3, "c", 0.0, 10),
            proc_at(1, "a", 50.0, 10),
        ];
        sort_processes(&mut shuffled, SortBy::Cpu);
        assert_eq!(
            shuffled.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn sorts_by_memory_when_asked() {
        let mut list = vec![proc_at(1, "a", 90.0, 10), proc_at(2, "b", 0.0, 999)];
        sort_processes(&mut list, SortBy::Memory);
        assert_eq!(list[0].pid, 2);
    }

    #[test]
    fn narrow_caps_the_list_but_keeps_the_true_count() {
        let metrics = Metrics {
            timestamp_ms: 0,
            cpu: CpuMetrics {
                usage: 0.0,
                core_count: 1,
                per_core: vec![0.0],
            },
            memory: MemoryMetrics {
                total: 1,
                used: 0,
                swap_total: 0,
                swap_used: 0,
            },
            host: HostMetrics {
                name: None,
                os: None,
                uptime_sec: 0,
                load_avg: [0.0; 3],
            },
            gpu: None,
            processes: (0..50).map(|i| proc_at(i, "p", i as f32, 0)).collect(),
            process_count: 50,
            truncated: false,
        };

        let narrowed = narrow(&metrics, SortBy::Cpu, 10);
        assert_eq!(narrowed.processes.len(), 10);
        assert!(narrowed.truncated);
        // The count must stay the machine's, not the page's — otherwise the UI
        // tells the user their host runs ten processes.
        assert_eq!(narrowed.process_count, 50);

        let all = narrow(&metrics, SortBy::Cpu, 200);
        assert_eq!(all.processes.len(), 50);
        assert!(!all.truncated);
    }

    #[test]
    fn top_n_is_clamped_rather_than_rejected() {
        let metrics = Metrics {
            timestamp_ms: 0,
            cpu: CpuMetrics {
                usage: 0.0,
                core_count: 1,
                per_core: vec![],
            },
            memory: MemoryMetrics {
                total: 1,
                used: 0,
                swap_total: 0,
                swap_used: 0,
            },
            host: HostMetrics {
                name: None,
                os: None,
                uptime_sec: 0,
                load_avg: [0.0; 3],
            },
            gpu: None,
            processes: (0..500).map(|i| proc_at(i, "p", 0.0, 0)).collect(),
            process_count: 500,
            truncated: false,
        };
        assert_eq!(
            narrow(&metrics, SortBy::Cpu, 100_000).processes.len(),
            MAX_TOP_N
        );
        assert_eq!(narrow(&metrics, SortBy::Cpu, 0).processes.len(), 1);
    }

    #[test]
    fn parses_signal_names_and_rejects_nonsense() {
        assert_eq!(KillSignal::parse("term"), Some(KillSignal::Term));
        assert_eq!(KillSignal::parse("SIGKILL"), Some(KillSignal::Kill));
        assert_eq!(KillSignal::parse("int"), Some(KillSignal::Int));
        assert_eq!(KillSignal::parse("hup"), Some(KillSignal::Hup));
        // Absent/empty means the default, and the default is TERM — not KILL.
        assert_eq!(KillSignal::parse(""), Some(KillSignal::Term));
        assert_eq!(KillSignal::parse("stop"), None);
        assert_eq!(KillSignal::parse("9"), None);
    }

    #[test]
    fn sort_by_parse_falls_back_to_cpu() {
        assert_eq!(SortBy::parse(Some("memory")), SortBy::Memory);
        assert_eq!(SortBy::parse(Some("cpu")), SortBy::Cpu);
        assert_eq!(SortBy::parse(Some("nonsense")), SortBy::Cpu);
        assert_eq!(SortBy::parse(None), SortBy::Cpu);
    }

    #[test]
    fn refuses_to_kill_itself_or_init() {
        // The server killing itself would look like a crash and take every
        // terminal with it (D32).
        let own = std::process::id();
        assert!(matches!(
            kill(own, KillSignal::Term),
            Err(KillError::Refused(_))
        ));
        assert!(matches!(
            kill(0, KillSignal::Term),
            Err(KillError::Refused(_))
        ));
        assert!(matches!(
            kill(1, KillSignal::Term),
            Err(KillError::Refused(_))
        ));
    }

    #[tokio::test]
    async fn a_sample_reports_plausible_invariants() {
        // Invariants, never fixed numbers: the values depend on the machine, but
        // these relations hold on every machine.
        let metrics = sample_twice().await;

        assert!(metrics.memory.total > 0);
        assert!(metrics.memory.used <= metrics.memory.total);
        assert!(metrics.memory.swap_used <= metrics.memory.swap_total);
        assert!(metrics.cpu.core_count > 0);
        assert_eq!(metrics.cpu.per_core.len(), metrics.cpu.core_count);
        assert!(metrics.timestamp_ms > 0);

        // Our own process must be in a listing of every process — the check that
        // catches an enumeration silently returning nothing.
        let own = std::process::id();
        assert!(
            metrics.processes.iter().any(|p| p.pid == own),
            "own pid {own} missing from {} processes",
            metrics.processes.len()
        );
    }

    #[tokio::test]
    async fn cpu_usage_is_not_zero_on_the_first_read() {
        // D23, and the whole reason for the two-refresh warm-up: a fresh `System`
        // reports 0.0% because usage is a delta with nothing to subtract from.
        // Busy work guarantees there is something to measure.
        let handle = tokio::task::spawn_blocking(|| {
            let start = std::time::Instant::now();
            let mut acc = 0u64;
            while start.elapsed() < MINIMUM_CPU_UPDATE_INTERVAL * 3 {
                acc = acc.wrapping_add(1);
            }
            acc
        });
        let metrics = sample_twice().await;
        let _ = handle.await;

        assert!(
            metrics.cpu.usage > 0.0,
            "cpu usage should be measurable, got {}",
            metrics.cpu.usage
        );
    }

    #[tokio::test]
    async fn one_sampler_serves_two_subscribers_and_stops_when_told() {
        let sampler = MetricsSampler::new();
        let _a = sampler.subscribe().await;
        let _b = sampler.subscribe().await;
        assert!(sampler.is_running().await);

        sampler.stop().await;
        assert!(!sampler.is_running().await);
    }
}
