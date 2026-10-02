use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

macro_rules! probes {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum Probe {
            $($variant),+
        }

        impl Probe {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }
    };
}

macro_rules! counters {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum Counter {
            $($variant),+
        }

        impl Counter {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }
    };
}

probes! {
    Analysis => "analysis",
    ProcessNeed => "process_need",
    ProcessParsed => "process_parsed",
    ResolveLexicalName => "resolve_lexical_name",
    EvaluateInstalledFunction => "evaluate_installed_function",
    ParsedSource => "parsed_source",
    BindingImage => "binding_image",
    OakParseContext => "oak_parse_context",
    NamespaceImports => "namespace_imports",
    PrepareConstructionImage => "prepare_construction_image",
    WorkerRequest => "worker_request",
    PackageFingerprint => "package_fingerprint",
    Finalize => "finalize",
    ExecuteConstruction => "execute_construction",
    SummaryKey => "summary_key_without_node",
    StoreBindingImage => "binding_image.store",
    ObjectsMerge => "binding_image.objects_merge",
    ImageExtend => "binding_image.image_extend",
    ProcessReferences => "process_references",
    ProcessCalls => "process_calls",
    GuardVerdict => "guard_verdict",
    ProcessEffects => "process_effects",
    LockWait => "lock_wait",
}

counters! {
    ConstructionEvaluations => "construction_evaluations",
    SummariesStored => "summaries_stored",
    SummaryEffectsStored => "summary_effects_stored",
    SummaryReadsStored => "summary_reads_stored",
    ConstructionMemoHits => "construction_memo_hits",
    ConstructionSummaryHits => "construction_summary_hits",
    SummaryRejectedArguments => "summary_rejected_unstable_arguments",
    SummaryRejectedCut => "summary_rejected_depth_cut",
    SummaryRejectedAssumption => "summary_rejected_recursion_assumption",
    SummaryRejectedWrites => "summary_rejected_graph_access",
    ConstructionPure => "construction_pure_evaluations",
    ConstructionImpure => "construction_impure_evaluations",
    LexicalMemoHits => "lexical_memo_hits",
    LexicalMemoMisses => "lexical_memo_misses",
    ParseMemoHits => "parse_memo_hits",
    ParseMemoMisses => "parse_memo_misses",
    BindingImageHits => "binding_image_hits",
    BindingLoadPrepare => "binding_load_prepare_construction",
    BindingLoadProcess => "binding_load_process_binding",
    BindingLoadConstruction => "binding_load_construction",
    BindingImageMisses => "binding_image_misses",
    NeedsStarted => "needs_started",
    LatticeJoins => "lattice_joins",
    LatticeGrowths => "lattice_growths",
    SccCount => "scc_count",
    SccLargest => "scc_largest",
    SccIterations => "scc_iterations",
    TransferExecutions => "transfer_executions",
    QueueDepthMax => "queue_depth_max",
    TaskSteals => "task_steals",
    WorkerBusyMicros => "worker_busy_micros",
    RWorkerStartups => "r_worker_startups",
    RRequests => "r_requests",
    RRequestBytes => "r_request_bytes",
    RResponseBytes => "r_response_bytes",
    RBatchItems => "r_batch_items",
    FingerprintOperations => "fingerprint_operations",
    FingerprintFiles => "fingerprint_files",
    FingerprintBytes => "fingerprint_bytes",
    QueriesRecomputed => "queries_recomputed",
    QueriesPruned => "queries_pruned_unchanged",
    QueriesReused => "queries_reused",
}

const PROBE_COUNT: usize = Probe::ALL.len();
const COUNTER_COUNT: usize = Counter::ALL.len();

struct Cell {
    calls: AtomicU64,
    inclusive_nanos: AtomicU64,
    exclusive_nanos: AtomicU64,
    exclusive_allocations: AtomicU64,
    exclusive_bytes: AtomicU64,
    unique: Mutex<HashSet<u64>>,
}

struct Registry {
    probes: Vec<Cell>,
    counters: Vec<AtomicU64>,
    opcodes: Mutex<std::collections::BTreeMap<String, (u64, u64)>>,
    started: Instant,
}

fn registry() -> Option<&'static Registry> {
    if !cfg!(feature = "profile") {
        return None;
    }
    static REGISTRY: OnceLock<Option<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            std::env::var_os("SLINKER_PROFILE")
                .filter(|value| !value.is_empty() && value != "0")
                .map(|_| Registry {
                    probes: (0..PROBE_COUNT)
                        .map(|_| Cell {
                            calls: AtomicU64::new(0),
                            inclusive_nanos: AtomicU64::new(0),
                            exclusive_nanos: AtomicU64::new(0),
                            exclusive_allocations: AtomicU64::new(0),
                            exclusive_bytes: AtomicU64::new(0),
                            unique: Mutex::new(HashSet::new()),
                        })
                        .collect(),
                    counters: (0..COUNTER_COUNT).map(|_| AtomicU64::new(0)).collect(),
                    opcodes: Mutex::new(std::collections::BTreeMap::new()),
                    started: Instant::now(),
                })
        })
        .as_ref()
}

#[must_use]
pub fn enabled() -> bool {
    registry().is_some()
}

struct Frame {
    probe: Probe,
    child_nanos: u64,
    child_allocations: u64,
    child_bytes: u64,
}

thread_local! {
    static STACK: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    static ACTIVE: RefCell<[u32; PROBE_COUNT]> = const { RefCell::new([0; PROBE_COUNT]) };
}

fn innermost_probe() -> Option<&'static str> {
    STACK
        .try_with(|stack| {
            stack
                .try_borrow()
                .ok()
                .and_then(|stack| stack.last().map(|frame| frame.probe.name()))
        })
        .ok()
        .flatten()
}

pub struct Span {
    live: Option<(Probe, Instant, u64, u64)>,
}

#[must_use]
pub fn span(probe: Probe) -> Span {
    let Some(registry) = registry() else {
        return Span { live: None };
    };
    registry.probes[probe as usize]
        .calls
        .fetch_add(1, Ordering::Relaxed);
    STACK.with(|stack| {
        stack.borrow_mut().push(Frame {
            probe,
            child_nanos: 0,
            child_allocations: 0,
            child_bytes: 0,
        });
    });
    ACTIVE.with(|active| active.borrow_mut()[probe as usize] += 1);
    Span {
        live: Some((
            probe,
            Instant::now(),
            heap::thread_allocations(),
            heap::thread_bytes(),
        )),
    }
}

#[must_use]
pub fn keyed_span(probe: Probe, key: &impl Hash) -> Span {
    if let Some(registry) = registry() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        registry.probes[probe as usize]
            .unique
            .lock()
            .expect("profile unique set")
            .insert(hasher.finish());
    }
    span(probe)
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((probe, start, allocations_before, bytes_before)) = self.live.take() else {
            return;
        };
        let Some(registry) = registry() else {
            return;
        };
        let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let allocations = heap::thread_allocations().saturating_sub(allocations_before);
        let bytes = heap::thread_bytes().saturating_sub(bytes_before);
        let (child, child_allocations, child_bytes) = STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let frame = stack.pop().map_or((0, 0, 0), |frame| {
                (
                    frame.child_nanos,
                    frame.child_allocations,
                    frame.child_bytes,
                )
            });
            if let Some(parent) = stack.last_mut() {
                parent.child_nanos = parent.child_nanos.saturating_add(elapsed);
                parent.child_allocations = parent.child_allocations.saturating_add(allocations);
                parent.child_bytes = parent.child_bytes.saturating_add(bytes);
            }
            frame
        });
        let outermost = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            active[probe as usize] -= 1;
            active[probe as usize] == 0
        });
        let cell = &registry.probes[probe as usize];
        if outermost {
            cell.inclusive_nanos.fetch_add(elapsed, Ordering::Relaxed);
        }
        cell.exclusive_nanos
            .fetch_add(elapsed.saturating_sub(child), Ordering::Relaxed);
        cell.exclusive_allocations.fetch_add(
            allocations.saturating_sub(child_allocations),
            Ordering::Relaxed,
        );
        cell.exclusive_bytes
            .fetch_add(bytes.saturating_sub(child_bytes), Ordering::Relaxed);
    }
}

pub fn count(counter: Counter) {
    add(counter, 1);
}

pub fn add(counter: Counter, amount: u64) {
    if let Some(registry) = registry() {
        registry.counters[counter as usize].fetch_add(amount, Ordering::Relaxed);
    }
}

pub fn max(counter: Counter, value: u64) {
    if let Some(registry) = registry() {
        registry.counters[counter as usize].fetch_max(value, Ordering::Relaxed);
    }
}

pub fn r_request(opcode: &str, request_bytes: usize, response_bytes: usize, nanos: u64) {
    if let Some(registry) = registry() {
        add(Counter::RRequests, 1);
        add(Counter::RRequestBytes, request_bytes as u64);
        add(Counter::RResponseBytes, response_bytes as u64);
        let mut opcodes = registry.opcodes.lock().expect("profile opcodes");
        let entry = opcodes.entry(opcode.to_owned()).or_default();
        entry.0 += 1;
        entry.1 += nanos;
    }
}

fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

#[must_use]
pub fn report() -> Option<String> {
    let registry = registry()?;
    let mut out = String::new();
    #[cfg(feature = "profile")]
    {
        out.push_str(&heap::summary());
        out.push_str(&heap::top_sites());
    }
    let wall = registry.started.elapsed();
    let _ = writeln!(
        out,
        "slinker profile (wall {:.0} ms)",
        wall.as_secs_f64() * 1e3
    );
    let _ = writeln!(
        out,
        "{:<30} {:>10} {:>10} {:>12} {:>12} {:>12} {:>12}",
        "probe", "calls", "unique", "incl ms", "excl ms", "excl allocs", "excl MiB"
    );
    for probe in Probe::ALL {
        let cell = &registry.probes[*probe as usize];
        let calls = cell.calls.load(Ordering::Relaxed);
        if calls == 0 {
            continue;
        }
        let unique = cell.unique.lock().expect("profile unique set").len();
        let unique = if unique == 0 {
            "-".to_owned()
        } else {
            unique.to_string()
        };
        let _ = writeln!(
            out,
            "{:<30} {:>10} {:>10} {:>12.1} {:>12.1} {:>12} {:>12.1}",
            probe.name(),
            calls,
            unique,
            millis(cell.inclusive_nanos.load(Ordering::Relaxed)),
            millis(cell.exclusive_nanos.load(Ordering::Relaxed)),
            cell.exclusive_allocations.load(Ordering::Relaxed),
            cell.exclusive_bytes.load(Ordering::Relaxed) as f64 / 1_048_576.0,
        );
    }
    let _ = writeln!(out, "counters");
    for counter in Counter::ALL {
        let value = registry.counters[*counter as usize].load(Ordering::Relaxed);
        if value != 0 {
            let _ = writeln!(out, "  {:<30} {value}", counter.name());
        }
    }
    let opcodes = registry.opcodes.lock().expect("profile opcodes");
    if !opcodes.is_empty() {
        let _ = writeln!(out, "r requests by opcode");
        for (opcode, (count, nanos)) in opcodes.iter() {
            let _ = writeln!(out, "  {opcode:<30} {count:>8} {:>10.1} ms", millis(*nanos));
        }
    }
    Some(out)
}

#[cfg(not(feature = "profile"))]
pub mod heap {
    pub fn thread_allocations() -> u64 {
        0
    }

    pub fn thread_bytes() -> u64 {
        0
    }
}

#[cfg(feature = "profile")]
pub mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static MILESTONE: AtomicUsize = AtomicUsize::new(0);
    static GROWTH: std::sync::Mutex<Vec<(usize, &'static str)>> = std::sync::Mutex::new(Vec::new());

    const MILESTONE_STEP: usize = 3 << 20;

    fn record_growth(live: usize) {
        let seen = MILESTONE.load(Ordering::Relaxed);
        if live < seen + MILESTONE_STEP
            || MILESTONE
                .compare_exchange(seen, live, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let probe = super::innermost_probe().unwrap_or("-");
        if let Ok(mut growth) = GROWTH.lock() {
            growth.push((live, probe));
        }
    }

    thread_local! {
        static THREAD_ALLOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static THREAD_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    pub fn thread_bytes() -> u64 {
        THREAD_BYTES.with(std::cell::Cell::get)
    }

    thread_local! {
        static SAMPLING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    static SITES: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
        std::sync::Mutex::new(None);

    const SAMPLE_EVERY: u64 = 2048;

    static SAMPLING_ENABLED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    pub fn enable_site_sampling() {
        SAMPLING_ENABLED.store(true, Ordering::Relaxed);
    }

    fn sample() {
        let entered = SAMPLING
            .try_with(|flag| !flag.replace(true))
            .unwrap_or(false);
        if !entered {
            return;
        }
        let trace = std::backtrace::Backtrace::force_capture().to_string();
        let site = trace
            .lines()
            .filter(|line| line.contains("slinker") && !line.contains("profile::heap"))
            .filter(|line| !line.trim_start().starts_with("at "))
            .take(4)
            .map(|line| line.trim().to_owned())
            .collect::<Vec<_>>()
            .join(" <- ");
        if let Ok(mut sites) = SITES.lock() {
            *sites
                .get_or_insert_with(Default::default)
                .entry(site)
                .or_default() += 1;
        }
        let _ = SAMPLING.try_with(|flag| flag.set(false));
    }

    pub fn sampling_requested() -> bool {
        std::env::var_os("SLINKER_ALLOC_SITES").is_some()
    }

    pub fn top_sites() -> String {
        let sites = SITES
            .lock()
            .ok()
            .and_then(|mut sites| sites.take())
            .unwrap_or_default();
        let mut sites = sites.into_iter().collect::<Vec<_>>();
        sites.sort_by_key(|site| std::cmp::Reverse(site.1));
        sites
            .into_iter()
            .take(40)
            .map(|(site, count)| {
                format!(
                    "{:>7} {site}
",
                    count * SAMPLE_EVERY
                )
            })
            .collect()
    }

    pub fn thread_allocations() -> u64 {
        THREAD_ALLOCATIONS.with(std::cell::Cell::get)
    }

    pub struct CountingAllocator;

    fn grew(by: usize) {
        let live = LIVE.fetch_add(by, Ordering::Relaxed) + by;
        PEAK.fetch_max(live, Ordering::Relaxed);
        record_growth(live);
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        let _ = THREAD_ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        let _ = THREAD_BYTES.try_with(|total| total.set(total.get() + by as u64));
        if SAMPLING_ENABLED.load(Ordering::Relaxed)
            && ALLOCATIONS
                .load(Ordering::Relaxed)
                .is_multiple_of(SAMPLE_EVERY)
        {
            sample();
        }
    }

    // SAFETY: every method forwards to `System` with the caller's layout and only adds relaxed counters.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            grew(layout.size());
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            grew(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            grew(new_size);
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    pub fn live_mib() -> f64 {
        LIVE.load(Ordering::Relaxed) as f64 / 1_048_576.0
    }

    pub fn summary() -> String {
        let growth = GROWTH
            .lock()
            .map(|growth| {
                growth
                    .iter()
                    .map(|(live, probe)| {
                        format!("{:.0} MiB in {probe}", *live as f64 / 1_048_576.0)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        format!(
            "heap: peak {:.1} MiB, live {:.1} MiB, {} allocations\nheap growth: {growth}\n",
            PEAK.load(Ordering::Relaxed) as f64 / 1_048_576.0,
            LIVE.load(Ordering::Relaxed) as f64 / 1_048_576.0,
            ALLOCATIONS.load(Ordering::Relaxed)
        )
    }
}
