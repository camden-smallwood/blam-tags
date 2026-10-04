//! Mutated synthetic tags: every read must return `Ok` or `Err`.
//!
//! The readers promise not to panic, overflow the stack, allocate the size of
//! a lie or hang on malformed input (`malformed_tags` pins the specific
//! defects that were fixed). This is the broad net behind those: a filled tag
//! of every group of every game (see `common::synthetic`), mutated by a seeded
//! generator — bytes flipped, words overwritten with boundary values, the file
//! truncated — and read through `TagFile::read_from_bytes` or, for Halo CE and
//! Halo 2, `classic::read_classic_tag_file`. A mutant that reads is also
//! walked field by field and written again, which must not panic either.
//!
//! An abort (stack overflow, failed allocation) cannot be caught in-process
//! and a hang would stall CI rather than fail it, so each game runs in a child
//! process: this binary re-run with `--exact mutation_worker`. The parent
//! kills a child that runs past its deadline and reports the last case the
//! child announced, which reproduces from the printed group and seed.
//!
//! `BLAM_MUTATIONS` overrides the number of mutants per group (default 48).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{BufRead, BufReader, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::synthetic::{self, FillPlan};

/// Records the largest single allocation, per thread, so a size read from the
/// file and trusted shows up even where the system would grant it lazily.
struct Counting;

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(layout.size())));
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(layout.size())));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(new_size)));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The largest allocation a read of a mutant may make. The synthetic tags are
/// at most a few hundred kilobytes, so anything near this is a size taken
/// from the file without checking it against the bytes that remain.
const ALLOCATION_LIMIT: usize = 64 << 20;

const WORKER_GAME: &str = "BLAM_MUTATION_WORKER_GAME";

/// A small, fixed generator: the same seed gives the same mutants on every
/// platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn seed_for(game: &str, group: &str, case: usize) -> u64 {
    // FNV-1a over the names, so a seed names one case of one group.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in game.bytes().chain(group.bytes()) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ (case as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1
}

/// One mutant of `original`, and a word saying what was done to it.
fn mutate(original: &[u8], seed: u64) -> (Vec<u8>, String) {
    let mut rng = Rng(seed);
    let mut bytes = original.to_vec();
    const BOUNDARIES: [u32; 7] = [0, 1, 0x7F, 0xFF, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF];
    match rng.below(4) {
        0 => {
            let flips = 1 + rng.below(4);
            for _ in 0..flips {
                let at = rng.below(bytes.len());
                bytes[at] ^= 1 << rng.below(8);
            }
            (bytes, format!("flip {flips}"))
        }
        1 => {
            let at = rng.below(bytes.len().saturating_sub(4) / 4) * 4;
            let value = BOUNDARIES[rng.below(BOUNDARIES.len())];
            let encoded = if rng.below(2) == 0 { value.to_le_bytes() } else { value.to_be_bytes() };
            bytes[at..at + 4].copy_from_slice(&encoded);
            (bytes, format!("word {value:#x} at {at}"))
        }
        2 => {
            let length = rng.below(bytes.len());
            bytes.truncate(length);
            (bytes, format!("truncate to {length}"))
        }
        _ => {
            // Shift a run of bytes: what a length that is off by a few does.
            let from = rng.below(bytes.len());
            let to = rng.below(bytes.len());
            let span = (1 + rng.below(64)).min(bytes.len() - from.max(to));
            bytes.copy_within(from..from + span, to);
            (bytes, format!("copy {span} from {from} to {to}"))
        }
    }
}

/// Read a mutant the way its game is read; a mutant that reads is walked
/// and written again. Returns whether it read.
fn exercise(game: &str, group: &str, bytes: &[u8]) -> bool {
    let Ok(tag) = synthetic::read_back(game, group, bytes) else { return false };
    let _ = synthetic::dump(&tag);
    let _ = tag.write_to_bytes();
    true
}

/// The child: every group of the game named by `WORKER_GAME`, mutated. Run
/// only by `mutated_synthetic_tags_never_crash_the_readers`.
#[test]
#[ignore = "a child process of mutated_synthetic_tags_never_crash_the_readers"]
fn mutation_worker() {
    let Ok(game) = std::env::var(WORKER_GAME) else { return };
    let mut out = std::io::stdout();
    match game.as_str() {
        // The harness's own checks: a child that dies and one that hangs.
        "abort" => {
            writeln!(out, "case abort").unwrap();
            out.flush().unwrap();
            std::process::abort();
        }
        "hang" => {
            writeln!(out, "case hang").unwrap();
            out.flush().unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        _ => {}
    }
    let per_group: usize = std::env::var("BLAM_MUTATIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(48);
    let plan = FillPlan { per_block: [1, 1, 0, 0], budget: 40 };
    let mut problems = Vec::new();
    let mut reads = 0usize;
    let mut cases = 0usize;
    for (_, group) in synthetic::groups(&game) {
        let original = synthetic::filled_bytes(&game, &group, plan);
        for case in 0..per_group {
            let seed = seed_for(&game, &group, case);
            let (bytes, what) = mutate(&original, seed);
            // Announced before the read, so a crash names its case.
            writeln!(out, "case {game} {group} seed {seed:#x}: {what}").unwrap();
            out.flush().unwrap();
            LARGEST.with(|largest| largest.set(0));
            let result = catch_unwind(AssertUnwindSafe(|| exercise(&game, &group, &bytes)));
            let largest = LARGEST.with(Cell::get);
            cases += 1;
            match result {
                Ok(read) => reads += usize::from(read),
                Err(panic) => {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    problems.push(format!("{group} seed {seed:#x} ({what}): panicked: {message}"));
                }
            }
            if largest > ALLOCATION_LIMIT {
                problems.push(format!("{group} seed {seed:#x} ({what}): allocated {largest} bytes"));
            }
        }
    }
    writeln!(out, "done {game}: {cases} mutants, {reads} still read").unwrap();
    // Some mutants must be refused and some must still read, or the
    // mutations were all one kind and the net tested little.
    assert!(reads > 0 && reads < cases, "{game}: {reads} of {cases} mutants read");
    assert!(problems.is_empty(), "{game}:\n{}", problems.join("\n"));
}

/// Run the worker for `game` in a child process. `Ok` with its last line if
/// it passed; `Err` describing the failure (exit status, or the deadline) and
/// the last case it announced otherwise.
fn run_worker(game: &str, deadline: Duration) -> Result<String, String> {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "mutation_worker", "--ignored", "--nocapture", "--test-threads=1"])
        .env(WORKER_GAME, game)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the worker");
    // Drain both pipes on threads so a chatty child can't block on a full one.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let last_case = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let last = last_case.clone();
    let out_thread = std::thread::spawn(move || {
        let mut done = String::new();
        // The first line follows libtest's "test mutation_worker ... ".
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(at) = line.find("case ") {
                *last.lock().unwrap() = line[at..].to_owned();
            } else if let Some(at) = line.find("done ") {
                done = line[at..].to_owned();
            }
        }
        done
    });
    let err_thread = std::thread::spawn(move || {
        let mut text = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            text.push_str(&line);
            text.push('\n');
        }
        text
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let done = out_thread.join().unwrap();
    let stderr = err_thread.join().unwrap();
    let last = last_case.lock().unwrap().clone();
    match status {
        Some(status) if status.success() => Ok(done),
        Some(status) => Err(format!("{game}: worker failed ({status}); last {last}\n{stderr}")),
        None => Err(format!("{game}: worker passed its {deadline:?} deadline; last {last}")),
    }
}

/// Every game's synthetic tags, mutated, in a child process per game. The
/// children run in parallel; each has a generous deadline, since a debug
/// build reads slowly, but a hang is still a failure rather than a stall.
#[test]
fn mutated_synthetic_tags_never_crash_the_readers() {
    let games: [&'static str; 8] = [
        "haloce_mcc",
        "halo2_mcc",
        "halo3_mcc",
        "halo3odst_mcc",
        "haloreach_mcc",
        "halo4_mcc",
        "halo2amp_mcc",
        "haloce_evolved",
    ];
    let handles: Vec<_> = games
        .iter()
        .map(|&game| std::thread::spawn(move || run_worker(game, Duration::from_secs(600))))
        .collect();
    let mut failures = Vec::new();
    for handle in handles {
        match handle.join().unwrap() {
            Ok(done) => eprintln!("{done}"),
            Err(failure) => failures.push(failure),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The net catches what it exists to catch: a child that aborts and a child
/// that never finishes are both reported, with the case they were on.
#[test]
fn the_harness_reports_an_abort_and_a_hang() {
    let abort = run_worker("abort", Duration::from_secs(60)).expect_err("an abort passed");
    assert!(abort.contains("worker failed") && abort.contains("case abort"), "{abort}");
    let hang = run_worker("hang", Duration::from_secs(3)).expect_err("a hang passed");
    assert!(hang.contains("deadline") && hang.contains("case hang"), "{hang}");
}
