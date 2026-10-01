use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Probe {
    Analysis,
    ProcessNeed,
    PreparseFrontier,
    ParsedSource,
    BindingImage,
    ResolveLexicalName,
    EvaluateInstalledFunction,
    ConstructionEvaluation,
    Finalize,
    PackageFingerprint,
    WorkerRequest,
}

impl Probe {
    const ALL: [Self; 11] = [
        Self::Analysis,
        Self::ProcessNeed,
        Self::PreparseFrontier,
        Self::ParsedSource,
        Self::BindingImage,
        Self::ResolveLexicalName,
        Self::EvaluateInstalledFunction,
        Self::ConstructionEvaluation,
        Self::Finalize,
        Self::PackageFingerprint,
        Self::WorkerRequest,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Analysis => "analysis",
            Self::ProcessNeed => "process_need",
            Self::PreparseFrontier => "preparse_frontier",
            Self::ParsedSource => "parsed_source",
            Self::BindingImage => "binding_image",
            Self::ResolveLexicalName => "resolve_lexical_name",
            Self::EvaluateInstalledFunction => "evaluate_installed_function",
            Self::ConstructionEvaluation => "construction_evaluation",
            Self::Finalize => "finalize",
            Self::PackageFingerprint => "package_fingerprint",
            Self::WorkerRequest => "worker_request",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Count {
    LexicalMemoHit,
    LexicalMemoMiss,
    ConstructionMemoHit,
    ConstructionMemoMiss,
    ParseMemoHit,
    LatticeJoin,
    LatticeGrowth,
    SccCount,
    SccLargest,
    SccIterations,
    TransferExecutions,
    PrunedRecomputations,
    QueueDepthMax,
    TaskSteals,
    RStartups,
    RRequests,
    RBatchItems,
    RProtocolBytes,
    FingerprintFiles,
    FingerprintBytes,
}

impl Count {
    const ALL: [Self; 20] = [
        Self::LexicalMemoHit,
        Self::LexicalMemoMiss,
        Self::ConstructionMemoHit,
        Self::ConstructionMemoMiss,
        Self::ParseMemoHit,
        Self::LatticeJoin,
        Self::LatticeGrowth,
        Self::SccCount,
        Self::SccLargest,
        Self::SccIterations,
        Self::TransferExecutions,
        Self::PrunedRecomputations,
        Self::QueueDepthMax,
        Self::TaskSteals,
        Self::RStartups,
        Self::RRequests,
        Self::RBatchItems,
        Self::RProtocolBytes,
        Self::FingerprintFiles,
        Self::FingerprintBytes,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::LexicalMemoHit => "lexical_memo_hit",
            Self::LexicalMemoMiss => "lexical_memo_miss",
            Self::ConstructionMemoHit => "construction_memo_hit",
            Self::ConstructionMemoMiss => "construction_memo_miss",
            Self::ParseMemoHit => "parse_memo_hit",
            Self::LatticeJoin => "lattice_join",
            Self::LatticeGrowth => "lattice_growth",
            Self::SccCount => "scc_count",
            Self::SccLargest => "scc_largest",
            Self::SccIterations => "scc_iterations",
            Self::TransferExecutions => "transfer_executions",
            Self::PrunedRecomputations => "pruned_recomputations",
            Self::QueueDepthMax => "queue_depth_max",
            Self::TaskSteals => "task_steals",
            Self::RStartups => "r_startups",
            Self::RRequests => "r_requests",
            Self::RBatchItems => "r_batch_items",
            Self::RProtocolBytes => "r_protocol_bytes",
            Self::FingerprintFiles => "fingerprint_files",
            Self::FingerprintBytes => "fingerprint_bytes",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Default)]
struct Totals {
    calls: AtomicU64,
    inclusive_nanos: AtomicU64,
    exclusive_nanos: AtomicU64,
}

struct Profiler {
    probes: [Totals; Probe::ALL.len()],
    counts: [AtomicU64; Count::ALL.len()],
    unique: Mutex<[HashSet<u64>; Probe::ALL.len()]>,
    opcodes: Mutex<std::collections::BTreeMap<&'static str, (u64, u64)>>,
    callees: Mutex<std::collections::BTreeMap<String, (u64, u64)>>,
    started: Instant,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static PROFILER: OnceLock<Profiler> = OnceLock::new();

pub fn enable() {
    PROFILER.get_or_init(|| Profiler {
        probes: Default::default(),
        counts: Default::default(),
        unique: Mutex::default(),
        opcodes: Mutex::default(),
        callees: Mutex::default(),
        started: Instant::now(),
    });
    ENABLED.store(true, Ordering::Release);
}

#[inline]
fn profiler() -> Option<&'static Profiler> {
    if ENABLED.load(Ordering::Relaxed) {
        PROFILER.get()
    } else {
        None
    }
}

struct Frame {
    probe: Probe,
    started: Instant,
    child_nanos: u64,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

#[must_use]
pub struct Timer {
    active: bool,
}

impl Drop for Timer {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Some(profiler) = profiler() else { return };
        STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let Some(frame) = stack.pop() else { return };
            let elapsed = u64::try_from(frame.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let totals = &profiler.probes[frame.probe.index()];
            totals.calls.fetch_add(1, Ordering::Relaxed);
            totals.inclusive_nanos.fetch_add(elapsed, Ordering::Relaxed);
            totals
                .exclusive_nanos
                .fetch_add(elapsed.saturating_sub(frame.child_nanos), Ordering::Relaxed);
            if let Some(parent) = stack.last_mut() {
                parent.child_nanos += elapsed;
            }
        });
    }
}

pub fn time(probe: Probe) -> Timer {
    if profiler().is_none() {
        return Timer { active: false };
    }
    STACK.with(|stack| {
        stack.borrow_mut().push(Frame {
            probe,
            started: Instant::now(),
            child_nanos: 0,
        });
    });
    Timer { active: true }
}

pub fn count(counter: Count, amount: u64) {
    if let Some(profiler) = profiler() {
        profiler.counts[counter.index()].fetch_add(amount, Ordering::Relaxed);
    }
}

pub fn count_max(counter: Count, value: u64) {
    if let Some(profiler) = profiler() {
        profiler.counts[counter.index()].fetch_max(value, Ordering::Relaxed);
    }
}

pub fn unique(probe: Probe, key: &impl Hash) {
    if let Some(profiler) = profiler() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        profiler.unique.lock().expect("profile mutex")[probe.index()].insert(hasher.finish());
    }
}

pub fn record_request(opcode: &'static str, bytes: usize) {
    if let Some(profiler) = profiler() {
        let mut opcodes = profiler.opcodes.lock().expect("profile mutex");
        let entry = opcodes.entry(opcode).or_default();
        entry.0 += 1;
        entry.1 += u64::try_from(bytes).unwrap_or(u64::MAX);
        count(Count::RRequests, 1);
        count(
            Count::RProtocolBytes,
            u64::try_from(bytes).unwrap_or(u64::MAX),
        );
    }
}

pub fn record_callee(callee: &str, nanos: u64) {
    if let Some(profiler) = profiler() {
        let mut callees = profiler.callees.lock().expect("profile mutex");
        let entry = callees.entry(callee.to_owned()).or_default();
        entry.0 += 1;
        entry.1 += nanos;
    }
}

pub fn report() -> Option<String> {
    let profiler = profiler()?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "profile (wall {:.3} s)",
        profiler.started.elapsed().as_secs_f64()
    );
    let unique = profiler.unique.lock().expect("profile mutex");
    let _ = writeln!(
        out,
        "{:<30} {:>10} {:>10} {:>12} {:>12}",
        "probe", "calls", "unique", "inclusive s", "exclusive s"
    );
    for probe in Probe::ALL {
        let totals = &profiler.probes[probe.index()];
        let calls = totals.calls.load(Ordering::Relaxed);
        if calls == 0 {
            continue;
        }
        let _ = writeln!(
            out,
            "{:<30} {:>10} {:>10} {:>12.3} {:>12.3}",
            probe.label(),
            calls,
            unique[probe.index()].len(),
            totals.inclusive_nanos.load(Ordering::Relaxed) as f64 / 1e9,
            totals.exclusive_nanos.load(Ordering::Relaxed) as f64 / 1e9,
        );
    }
    let _ = writeln!(out, "counters");
    for counter in Count::ALL {
        let value = profiler.counts[counter.index()].load(Ordering::Relaxed);
        if value != 0 {
            let _ = writeln!(out, "  {:<28} {value}", counter.label());
        }
    }
    let opcodes = profiler.opcodes.lock().expect("profile mutex");
    if !opcodes.is_empty() {
        let _ = writeln!(out, "r requests by opcode (count, bytes)");
        for (opcode, (calls, bytes)) in opcodes.iter() {
            let _ = writeln!(out, "  {opcode:<28} {calls:>8} {bytes:>12}");
        }
    }
    let callees = profiler.callees.lock().expect("profile mutex");
    if !callees.is_empty() {
        let mut top = callees.iter().collect::<Vec<_>>();
        top.sort_by(|left, right| right.1.0.cmp(&left.1.0).then_with(|| left.0.cmp(right.0)));
        let _ = writeln!(out, "top callees by evaluation count (count, exclusive s)");
        for (callee, (calls, nanos)) in top.into_iter().take(15) {
            let _ = writeln!(
                out,
                "  {callee:<40} {calls:>8} {:>10.3}",
                *nanos as f64 / 1e9
            );
        }
    }
    Some(out)
}
