use super::{Claim, Machine};

fn drain(machine: &Machine<u32>) -> Vec<u32> {
    machine.take_injected()
}

#[test]
fn a_key_is_queued_once_however_often_it_is_requested() {
    let machine = Machine::<u32>::default();
    machine.request(1);
    machine.request(1);
    machine.request(1);
    assert_eq!(drain(&machine), [1]);
    assert_eq!(machine.pending(), 1);
    assert!(machine.begin(&1));
    machine.release_claim(&1);
    machine.request(1);
    assert!(drain(&machine).is_empty());
    assert!(machine.is_quiescent());
}

#[test]
fn pending_never_reaches_zero_while_a_child_is_published_after_the_parent_decrement_point() {
    let machine = Machine::<u32>::default();
    machine.request(1);
    drain(&machine);
    assert!(machine.begin(&1));
    machine.request(2);
    machine.request(3);
    assert_eq!(
        machine.pending(),
        3,
        "children are counted before the parent finishes"
    );
    assert!(
        drain(&machine).is_empty(),
        "children are not visible to other workers while the parent runs"
    );
    machine.publish_staged();
    machine.release_claim(&1);
    assert_eq!(machine.pending(), 2);
    assert!(!machine.is_quiescent());
    for child in drain(&machine) {
        assert!(machine.begin(&child));
        machine.release_claim(&child);
    }
    assert!(machine.is_quiescent());
}

#[test]
fn an_inline_claim_takes_a_queued_key_and_the_queued_task_then_skips_it() {
    let machine = Machine::<u32>::default();
    machine.request(1);
    assert_eq!(machine.claim(&1), Claim::Mine);
    assert_eq!(
        machine.claim(&1),
        Claim::AlreadyDone,
        "re-entrant by the owner"
    );
    machine.release_claim(&1);
    assert_eq!(machine.claim(&1), Claim::AlreadyDone);
    for key in drain(&machine) {
        assert!(
            !machine.begin(&key),
            "the queued task finds the key already done"
        );
    }
    assert!(machine.is_quiescent());
}

#[test]
fn a_claim_by_another_thread_must_be_awaited() {
    let machine = std::sync::Arc::new(Machine::<u32>::default());
    assert_eq!(machine.claim(&7), Claim::Mine);
    let waiter = {
        let machine = std::sync::Arc::clone(&machine);
        std::thread::spawn(move || {
            assert_eq!(machine.claim(&7), Claim::Wait);
            machine.wait_done(&7);
            machine.claim(&7)
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(20));
    machine.release_claim(&7);
    assert_eq!(waiter.join().unwrap(), Claim::AlreadyDone);
}

#[test]
fn cyclic_waits_are_broken_instead_of_deadlocking() {
    let machine = std::sync::Arc::new(Machine::<u32>::default());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let spawn = |mine: u32, theirs: u32| {
        let machine = std::sync::Arc::clone(&machine);
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            assert_eq!(machine.claim(&mine), Claim::Mine);
            barrier.wait();
            if machine.claim(&theirs) == Claim::Wait {
                machine.wait_done(&theirs);
            }
            machine.release_claim(&mine);
        })
    };
    let first = spawn(1, 2);
    let second = spawn(2, 1);
    first.join().unwrap();
    second.join().unwrap();
    assert!(machine.is_quiescent());
}
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

struct Graph {
    machine: Machine<u32>,
    children: Vec<Vec<(u32, bool)>>,
    processed: Vec<std::sync::atomic::AtomicUsize>,
}

impl Graph {
    fn random(seed: u64) -> Self {
        let mut rng = Xorshift(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let keys = 8 + rng.below(56);
        let children = (0..keys)
            .map(|_| {
                (0..rng.below(5))
                    .map(|_| (rng.below(keys) as u32, rng.below(3) == 0))
                    .collect()
            })
            .collect();
        Self {
            machine: Machine::default(),
            children,
            processed: (0..keys)
                .map(|_| std::sync::atomic::AtomicUsize::new(0))
                .collect(),
        }
    }

    fn process(&self, key: u32) {
        self.processed[key as usize].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        for &(child, inline) in &self.children[key as usize] {
            if !inline {
                self.machine.request(child);
                continue;
            }
            match self.machine.claim(&child) {
                Claim::Mine => {
                    self.process(child);
                    self.machine.release_claim(&child);
                }
                Claim::Wait => self.machine.wait_done(&child),
                Claim::AlreadyDone => {}
            }
        }
    }

    fn spawn_injected<'scope>(&'scope self, scope: &rayon::Scope<'scope>) {
        for key in self.machine.take_injected() {
            scope.spawn(move |scope| {
                if self.machine.begin(&key) {
                    self.process(key);
                    self.machine.publish_staged();
                    self.spawn_injected(scope);
                    self.machine.release_claim(&key);
                }
                self.spawn_injected(scope);
            });
        }
    }

    fn reachable_from(&self, start: u32) -> Vec<bool> {
        let mut seen = vec![false; self.children.len()];
        let mut pending = vec![start];
        while let Some(key) = pending.pop() {
            if std::mem::replace(&mut seen[key as usize], true) {
                continue;
            }
            pending.extend(self.children[key as usize].iter().map(|(child, _)| *child));
        }
        seen
    }
}

#[test]
fn random_dependency_graphs_run_every_reachable_key_exactly_once_and_quiesce() {
    for seed in 0..96 {
        let graph = Graph::random(seed);
        graph.machine.request(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1 + (seed % 8) as usize)
            .build()
            .expect("pool");
        for _round in 0..64 {
            if graph.machine.is_quiescent() {
                break;
            }
            pool.install(|| rayon::scope(|scope| graph.spawn_injected(scope)));
        }
        let reachable = graph.reachable_from(0);
        for (key, counter) in graph.processed.iter().enumerate() {
            let runs = counter.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                runs,
                usize::from(reachable[key]),
                "seed {seed}: key {key} ran {runs} times (reachable: {})",
                reachable[key]
            );
        }
        assert!(graph.machine.is_quiescent(), "seed {seed} did not quiesce");
    }
}
