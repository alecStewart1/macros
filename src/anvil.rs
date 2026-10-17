//! ANVIL — an interactive Steel REPL backed by a *second*, fully independent
//! Steel interpreter.
//!
//! The editor already embeds one `Engine` (its config/scripting engine, see
//! [`crate::script`]). ANVIL runs a separate `Engine` on its own thread so REPL
//! experiments live in their own image: user `define`s never pollute the config
//! engine, the image can be restarted from scratch, and — crucially — a runaway
//! evaluation can be *interrupted* without taking the editor down.
//!
//! Interruption is cooperative but free: Steel's VM checks an atomic interrupt
//! flag on every opcode (`ThreadStateController`). So any pure-Scheme infinite
//! loop or unbounded recursion — `(define (spin) (spin))` included — unwinds
//! with an error at its next instruction once [`AnvilEngine::interrupt`] flips
//! the flag from the UI thread. Code stuck inside a blocking native call
//! (`time/sleep-ms`, a read from stdin) executes no opcodes and can't be
//! interrupted; [`AnvilEngine::reset`] handles that case by abandoning the
//! wedged thread and starting a fresh image on a new one.
//!
//! The plumbing mirrors [`crate::script::ScriptEngine`]: the `!Send` engine is
//! built on and never leaves its thread; only plain data crosses the channels.
//! Jobs are delivered over a blocking `std::sync::mpsc` channel (the thread
//! `recv()`s), and results come back over a `futures` channel the editor drains
//! on its foreground task. The thread is spawned lazily on the first job, so an
//! editor that never uses ANVIL never builds its interpreter.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use steel::rvals::SteelVal;
use steel::steel_vm::ThreadStateController;
use steel::steel_vm::engine::Engine;

use crate::script::{format_value, steel_error_string};

/// Stack for the ANVIL thread. Steel's compiler and macro expander recurse on
/// the shape of the source, so deeply nested input needs more than the 2 MiB a
/// spawned thread gets by default (overflowing aborts the whole editor).
const ANVIL_STACK_BYTES: usize = 256 * 1024 * 1024;

/// Most captured `(display …)` output returned per evaluation; a print loop
/// stopped by an interrupt can have buffered far more than a buffer should hold.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// Something the ANVIL engine thread sends back to the editor's foreground drain.
pub enum AnvilOutcome {
    /// A finished eval or introspection query, to be rendered by a Scheme
    /// callback in the *main* engine.
    Result {
        /// The callback that should render this result into the REPL buffer.
        callback: String,
        /// The printed value (`None` for void/no value), or the error text if
        /// the eval failed — including `"Interrupted by user"` when the
        /// evaluation was cancelled.
        result: Result<Option<String>, String>,
        /// Anything the form wrote to `(current-output-port)` — `(display …)`
        /// and friends — captured so it lands in the REPL buffer instead of the
        /// process stdout. `None` for queries and for evals that printed nothing.
        output: Option<String>,
    },
    /// Image-backed completion candidates (SLIME symbol completion): the live
    /// globals matching the prefix, plus the buffer byte range they replace.
    Completions {
        start: usize,
        end: usize,
        items: Vec<String>,
    },
}

/// An image-introspection request — the "mini-Swank" surface. Each is answered
/// against the *live* ANVIL image (its actual bindings, docs, macros), unlike the
/// editor's LSP which analyses source with a separate compile-only engine.
pub enum AnvilQuery {
    /// Live globals whose name contains the pattern (SLIME `apropos`).
    Apropos(String),
    /// Docstring for an identifier (SLIME `C-c C-d d`).
    Doc(String),
    /// Fully macroexpand a form (SLIME `C-c RET`).
    Macroexpand(String),
    /// One-line signature/summary for an identifier, for eldoc (SLIME echo-area
    /// hint). The first non-empty line of the image's docstring, which by Steel's
    /// `@doc` convention leads with the call signature.
    Signature(String),
}

enum AnvilJob {
    /// Evaluate `source`; deliver the result to `callback`.
    Eval { source: String, callback: String },
    /// Introspect the live image; deliver the rendered result to `callback`.
    Query { kind: AnvilQuery, callback: String },
    /// Compute completion candidates for `prefix` from the live image; deliver
    /// them (with the replace range) to the completion popup.
    Complete {
        prefix: String,
        start: usize,
        end: usize,
    },
}

/// A handle to the ANVIL REPL interpreter. Cheap to hold; the image itself
/// lives on the engine thread, which is spawned on first use.
pub struct AnvilEngine {
    /// Where every worker thread delivers its outcomes (cloned per spawn).
    outcomes: UnboundedSender<AnvilOutcome>,
    /// The current worker, or `None` before first use and after a reset.
    worker: Mutex<Option<Worker>>,
    /// Sequence number of the last job sent (jobs are numbered from 1).
    last_seq: AtomicU64,
}

/// One engine thread: its job queue plus the state the UI thread shares with it.
struct Worker {
    jobs: Sender<(u64, AnvilJob)>,
    shared: Arc<WorkerShared>,
}

/// State shared between the UI thread and one engine thread.
#[derive(Default)]
struct WorkerShared {
    /// The live engine's interrupt controller — `None` until the thread has
    /// built its engine. Nothing can be running before then, so an interrupt
    /// arriving early has nothing to cancel.
    controller: Mutex<Option<ThreadStateController>>,
    /// Set while a job runs, so callers can skip work that would only queue
    /// behind a long evaluation (eldoc lookups).
    busy: AtomicBool,
    /// Set by [`AnvilEngine::reset`]: the thread must exit after its current
    /// job and deliver nothing more — its image has been replaced.
    abandoned: AtomicBool,
    /// Jobs numbered at or below this were interrupted: one still waiting to
    /// start is cancelled instead of run. This is what makes an interrupt sent
    /// before the thread has even built its engine (no controller to flip yet)
    /// still stop the evaluation it was aimed at.
    cancel_upto: AtomicU64,
}

impl WorkerShared {
    fn interrupt(&self) {
        if let Some(c) = self.controller.lock().unwrap().as_ref() {
            c.interrupt();
        }
    }

    /// Clear the VM's interrupt flag so the next run starts cleanly.
    fn resume(&self) {
        if let Some(c) = self.controller.lock().unwrap().as_ref() {
            c.resume();
        }
    }
}

impl AnvilEngine {
    /// A handle plus the receiver of async outcomes (the editor drains it on a
    /// foreground task). No thread or interpreter exists until the first job.
    pub fn new() -> (Self, UnboundedReceiver<AnvilOutcome>) {
        let (outcomes, outcomes_rx) = unbounded::<AnvilOutcome>();
        let engine = Self {
            outcomes,
            worker: Mutex::new(None),
            last_seq: AtomicU64::new(0),
        };
        (engine, outcomes_rx)
    }

    /// Hand `job` to the engine thread, spawning one if there is none or the
    /// previous one died (a panic it couldn't recover from).
    fn send(&self, job: AnvilJob) {
        let mut worker = self.worker.lock().unwrap();
        let seq = self.last_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let job = match worker.as_ref() {
            Some(w) => match w.jobs.send((seq, job)) {
                Ok(()) => return,
                Err(unsent) => unsent.0.1,
            },
            None => job,
        };
        match spawn_worker(self.outcomes.clone()) {
            Ok(w) => {
                let _ = w.jobs.send((seq, job));
                *worker = Some(w);
            }
            Err(e) => {
                if let Some(callback) = job.callback() {
                    let _ = self.outcomes.unbounded_send(AnvilOutcome::Result {
                        callback,
                        result: Err(format!("could not start the ANVIL interpreter: {e}")),
                        output: None,
                    });
                }
            }
        }
    }

    /// Queue a REPL evaluation. The printed result is delivered to `callback`
    /// (a function in the main engine) via the outcome channel.
    pub fn eval(&self, source: &str, callback: &str) {
        self.send(AnvilJob::Eval {
            source: source.to_string(),
            callback: callback.to_string(),
        });
    }

    /// Queue an image-introspection query; the rendered result is delivered to
    /// `callback` (a function in the main engine) via the outcome channel.
    pub fn query(&self, kind: AnvilQuery, callback: &str) {
        self.send(AnvilJob::Query {
            kind,
            callback: callback.to_string(),
        });
    }

    /// Queue an image-backed completion: the live globals starting with
    /// `prefix`, to replace the buffer range `start..end`. The result arrives as
    /// [`AnvilOutcome::Completions`] for the editor's completion popup.
    pub fn complete(&self, prefix: &str, start: usize, end: usize) {
        self.send(AnvilJob::Complete {
            prefix: prefix.to_string(),
            start,
            end,
        });
    }

    /// Whether the engine thread is in the middle of a job.
    pub fn is_busy(&self) -> bool {
        self.worker
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|w| w.shared.busy.load(Ordering::Acquire))
    }

    /// Cancel the current evaluation (SLIME-style `C-c C-c`). Sets the VM's
    /// interrupt flag; the running eval unwinds at its next opcode. Safe to call
    /// when nothing is running — the next eval clears the flag first.
    pub fn interrupt(&self) {
        if let Some(w) = self.worker.lock().unwrap().as_ref() {
            w.shared
                .cancel_upto
                .fetch_max(self.last_seq.load(Ordering::Acquire), Ordering::AcqRel);
            w.shared.interrupt();
        }
    }

    /// Throw the image away (SLIME's restart-inferior-lisp): the next job runs
    /// in a fresh interpreter with no user definitions. The current thread is
    /// interrupted and abandoned rather than asked to rebuild, so this works
    /// even when it is wedged in a blocking native call that no interrupt can
    /// reach; it exits on its own once that call returns, delivering nothing.
    pub fn reset(&self) {
        if let Some(w) = self.worker.lock().unwrap().take() {
            w.shared.abandoned.store(true, Ordering::Release);
            w.shared.interrupt();
        }
    }
}

impl AnvilJob {
    /// The callback a job's result goes to, for jobs that report one.
    fn callback(&self) -> Option<String> {
        match self {
            AnvilJob::Eval { callback, .. } | AnvilJob::Query { callback, .. } => {
                Some(callback.clone())
            }
            AnvilJob::Complete { .. } => None,
        }
    }
}

/// Start an engine thread (its interpreter is built on the thread itself).
fn spawn_worker(outcomes: UnboundedSender<AnvilOutcome>) -> std::io::Result<Worker> {
    let (jobs, jobs_rx) = channel::<(u64, AnvilJob)>();
    let shared = Arc::new(WorkerShared::default());
    let thread_shared = shared.clone();
    std::thread::Builder::new()
        .name("anvil".into())
        .stack_size(ANVIL_STACK_BYTES)
        .spawn(move || anvil_thread(jobs_rx, outcomes, thread_shared))?;
    Ok(Worker { jobs, shared })
}

/// Build a fresh interpreter and publish its interrupt controller.
fn fresh_engine(shared: &WorkerShared) -> Engine {
    let engine = Engine::new();
    *shared.controller.lock().unwrap() = Some(engine.get_thread_state_controller());
    engine
}

/// The text of a caught panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// The REPL engine thread: owns the `!Send` `Engine` for its whole life,
/// processing jobs FIFO until its handle is dropped or it is abandoned.
fn anvil_thread(
    jobs: Receiver<(u64, AnvilJob)>,
    outcomes: UnboundedSender<AnvilOutcome>,
    shared: Arc<WorkerShared>,
) {
    let mut engine = fresh_engine(&shared);
    while let Ok((seq, job)) = jobs.recv() {
        if shared.abandoned.load(Ordering::Acquire) {
            return;
        }
        shared.busy.store(true, Ordering::Release);
        let callback = job.callback();
        // Clear the VM's interrupt flag (one sent while nothing ran), *then*
        // check for a cancellation aimed at this job: an interrupt landing
        // between the two is caught by the check, one after it by the flag.
        shared.resume();
        if seq <= shared.cancel_upto.load(Ordering::Acquire) {
            shared.busy.store(false, Ordering::Release);
            if let Some(callback) = callback {
                let _ = outcomes.unbounded_send(AnvilOutcome::Result {
                    callback,
                    result: Err("Interrupted by user".into()),
                    output: None,
                });
            }
            continue;
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| run_job(&mut engine, &shared, job)));
        shared.busy.store(false, Ordering::Release);
        if shared.abandoned.load(Ordering::Acquire) {
            return;
        }
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(payload) => {
                // A panic inside Steel can leave the image inconsistent: report
                // it and carry on with a fresh one rather than dying silently.
                engine = fresh_engine(&shared);
                callback.map(|callback| AnvilOutcome::Result {
                    callback,
                    result: Err(format!(
                        "the interpreter panicked ({}); the image was restarted",
                        panic_message(payload.as_ref())
                    )),
                    output: None,
                })
            }
        };
        if let Some(outcome) = outcome {
            let _ = outcomes.unbounded_send(outcome);
        }
    }
}

/// Run one job against the image, producing the outcome to deliver.
fn run_job(engine: &mut Engine, shared: &WorkerShared, job: AnvilJob) -> Option<AnvilOutcome> {
    match job {
        AnvilJob::Eval { source, callback } => {
            // Redirect this eval's output to a fresh in-image string port so
            // `(display …)` lands in the REPL, not the process stdout. Run as its
            // own top-level form *before* the user's source so the user's
            // `define`s stay top-level (wrapping them in a `let` wouldn't).
            let _ = engine.run("(current-output-port (open-output-string))");
            let result = engine
                .run(source)
                .map(|values| values.last().and_then(format_value))
                .map_err(|e| steel_error_string(&e));
            // The run may have ended via interrupt, which leaves the VM in the
            // Interrupted state — reset it so the drain below (and the next
            // eval) isn't immediately poisoned.
            shared.resume();
            let output = drain_output(engine);
            Some(AnvilOutcome::Result {
                callback,
                result,
                output,
            })
        }
        AnvilJob::Query { kind, callback } => {
            let value = run_query(engine, kind);
            Some(AnvilOutcome::Result {
                callback,
                result: Ok(Some(value)),
                output: None,
            })
        }
        AnvilJob::Complete { prefix, start, end } => Some(AnvilOutcome::Completions {
            start,
            end,
            items: complete_globals(engine, &prefix),
        }),
    }
}

/// Read back (and clear) whatever the last eval wrote to the redirected
/// `current-output-port`. `None` when nothing was printed or the port couldn't
/// be read (e.g. a user who reassigned `current-output-port`).
fn drain_output(engine: &mut Engine) -> Option<String> {
    let vals = engine
        .run("(get-output-string (current-output-port))")
        .ok()?;
    match vals.into_iter().next_back()? {
        SteelVal::StringV(s) if !s.is_empty() => Some(truncate_output(&s)),
        _ => None,
    }
}

/// Cap captured output at [`MAX_OUTPUT_BYTES`], keeping the head and noting how
/// much was dropped.
fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_string();
    }
    let cut = s.floor_char_boundary(MAX_OUTPUT_BYTES);
    format!(
        "{}\n[… {} more bytes of output dropped]\n",
        &s[..cut],
        s.len() - cut
    )
}

/// The live image's global bindings whose name passes `keep`, sorted and
/// de-duplicated. Reads `readable_globals`, so it sees everything the user has
/// defined this session, immediately.
fn matching_globals(engine: &Engine, keep: impl Fn(&str) -> bool) -> Vec<String> {
    let mut names: Vec<String> = engine
        .readable_globals(0)
        .iter()
        .map(|sym| sym.resolve().to_string())
        .filter(|name| keep(name))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The live globals starting with `prefix` — the candidate set for
/// SLIME-style symbol completion.
fn complete_globals(engine: &Engine, prefix: &str) -> Vec<String> {
    matching_globals(engine, |name| name.starts_with(prefix))
}

/// Answer an [`AnvilQuery`] against the live image, rendered as display text.
fn run_query(engine: &mut Engine, kind: AnvilQuery) -> String {
    match kind {
        AnvilQuery::Apropos(pattern) => {
            let names = matching_globals(engine, |name| name.contains(&pattern));
            if names.is_empty() {
                format!(";; no live binding matches {pattern:?}")
            } else {
                names.join("\n")
            }
        }
        AnvilQuery::Doc(name) => engine
            .get_doc_for_identifier(&name)
            .unwrap_or_else(|| format!(";; {name}: no documentation in the image")),
        AnvilQuery::Macroexpand(form) => {
            // Without optimizations: show the *macro* expansion, not the
            // optimizer's constant-folded output (`(when #t 1)` should reveal the
            // `if`, not fold to `1`).
            match engine.emit_expanded_ast_without_optimizations(&form, None) {
                Ok(exprs) => exprs
                    .into_iter()
                    .map(|expr| expr.to_pretty(60))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                Err(e) => format!(";; macroexpand error: {}", steel_error_string(&e)),
            }
        }
        AnvilQuery::Signature(name) => {
            if let Some(doc) = engine.get_doc_for_identifier(&name) {
                // eldoc wants one line: the docstring's first non-empty line.
                doc.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{name}: documented but empty"))
            } else if engine
                .readable_globals(0)
                .iter()
                .any(|sym| sym.resolve() == name)
            {
                format!("{name}: bound (no documentation in the image)")
            } else {
                format!("{name}: not bound in the image")
            }
        }
    }
}

#[cfg(test)]
impl AnvilOutcome {
    fn callback(&self) -> &str {
        match self {
            AnvilOutcome::Result { callback, .. } => callback,
            _ => "",
        }
    }
    fn value(&self) -> Option<&str> {
        match self {
            AnvilOutcome::Result {
                result: Ok(value), ..
            } => value.as_deref(),
            _ => None,
        }
    }
    fn error(&self) -> Option<&str> {
        match self {
            AnvilOutcome::Result {
                result: Err(error), ..
            } => Some(error),
            _ => None,
        }
    }
    fn output(&self) -> Option<&str> {
        match self {
            AnvilOutcome::Result { output, .. } => output.as_deref(),
            _ => None,
        }
    }
    fn completions(&self) -> &[String] {
        match self {
            AnvilOutcome::Completions { items, .. } => items,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::executor::block_on;
    use std::time::Duration;

    #[test]
    fn evaluates_and_returns_the_last_value() {
        let (engine, mut outcomes) = AnvilEngine::new();
        engine.eval("(define x 41) (+ x 1)", "cb");
        let o = block_on(outcomes.next()).unwrap();
        assert_eq!(o.callback(), "cb");
        assert_eq!(o.value(), Some("42"));
        assert!(o.error().is_none());
    }

    #[test]
    fn captures_printed_output_alongside_the_value() {
        let (engine, mut outcomes) = AnvilEngine::new();
        engine.eval(r#"(displayln "hello") (+ 2 3)"#, "cb");
        let o = block_on(outcomes.next()).unwrap();
        assert_eq!(o.value(), Some("5"));
        assert_eq!(
            o.output(),
            Some("hello\n"),
            "(displayln …) should be captured, not sent to process stdout"
        );
        // A fresh eval starts with an empty capture buffer (no accumulation).
        engine.eval("(+ 1 1)", "cb");
        let o2 = block_on(outcomes.next()).unwrap();
        assert_eq!(o2.value(), Some("2"));
        assert!(o2.output().is_none(), "got stale output {:?}", o2.output());
    }

    #[test]
    fn query_apropos_and_macroexpand_hit_the_live_image() {
        let (engine, mut outcomes) = AnvilEngine::new();
        // A user define becomes visible to apropos against the live image.
        engine.eval("(define my-anvil-thing 1)", "cb");
        let _ = block_on(outcomes.next()).unwrap();
        engine.query(AnvilQuery::Apropos("my-anvil-thing".into()), "cb");
        let o = block_on(outcomes.next()).unwrap();
        assert_eq!(o.value(), Some("my-anvil-thing"));
        // Macroexpand resolves against the image's macros.
        engine.query(AnvilQuery::Macroexpand("(when #t 1)".into()), "cb");
        let m = block_on(outcomes.next()).unwrap();
        assert!(
            m.value().unwrap_or_default().contains("if"),
            "`when` should expand to an `if`, got {:?}",
            m.value()
        );
    }

    #[test]
    fn signature_reports_live_image_bindings() {
        let (engine, mut outcomes) = AnvilEngine::new();
        // A session define is "bound" to eldoc even without a docstring — the
        // whole point of querying the live image rather than static source.
        engine.eval("(define (sig-fn a b) (+ a b))", "cb");
        let _ = block_on(outcomes.next()).unwrap();
        engine.query(AnvilQuery::Signature("sig-fn".into()), "cb");
        let bound = block_on(outcomes.next()).unwrap();
        assert_eq!(
            bound.value(),
            Some("sig-fn: bound (no documentation in the image)")
        );
        // An unknown symbol is reported as unbound, not silently empty.
        engine.query(AnvilQuery::Signature("no-such-binding".into()), "cb");
        let unbound = block_on(outcomes.next()).unwrap();
        assert_eq!(
            unbound.value(),
            Some("no-such-binding: not bound in the image")
        );
    }

    #[test]
    fn completion_sees_user_defines_in_the_live_image() {
        let (engine, mut outcomes) = AnvilEngine::new();
        // A built-in symbol is completable out of the box…
        engine.complete("string->", 0, 8);
        let o = block_on(outcomes.next()).unwrap();
        assert!(
            o.completions().iter().any(|c| c == "string->number"),
            "expected a built-in like string->number, got {:?}",
            o.completions()
        );
        // …and a user define shows up immediately (the whole point vs. static LSP).
        engine.eval("(define (anvil-greeter who) who)", "cb");
        let _ = block_on(outcomes.next()).unwrap();
        engine.complete("anvil-greet", 0, 11);
        let o = block_on(outcomes.next()).unwrap();
        assert_eq!(o.completions(), &["anvil-greeter".to_string()]);
    }

    #[test]
    fn interrupt_unwinds_a_runaway_loop_without_killing_the_engine() {
        let (engine, mut outcomes) = AnvilEngine::new();
        // Raw tail recursion — the case a Scheme-level loop guard can't catch.
        engine.eval("(define (spin) (spin)) (spin)", "cb");
        // Let the loop spin up, then interrupt from this (UI) thread.
        std::thread::sleep(Duration::from_millis(300));
        engine.interrupt();
        let o = block_on(outcomes.next()).unwrap();
        assert!(
            o.error().is_some(),
            "interrupt should surface as an error, got value {:?}",
            o.value()
        );
        // The engine survives the interrupt: a later eval still works.
        engine.eval("(+ 1 2)", "cb2");
        let next = block_on(outcomes.next()).unwrap();
        assert_eq!(next.callback(), "cb2");
        assert_eq!(next.value(), Some("3"));
        assert!(next.error().is_none());
    }

    #[test]
    fn reset_rebuilds_a_fresh_image() {
        let (engine, mut outcomes) = AnvilEngine::new();
        engine.eval("(define secret 99) secret", "cb");
        assert_eq!(block_on(outcomes.next()).unwrap().value(), Some("99"));
        engine.reset();
        // After a reset the binding is gone, so referencing it errors.
        engine.eval("secret", "cb");
        let o = block_on(outcomes.next()).unwrap();
        assert!(
            o.error().is_some(),
            "binding should be unbound after reset, got {:?}",
            o.value()
        );
    }

    #[test]
    fn reset_recovers_from_a_blocking_native_call() {
        let (engine, mut outcomes) = AnvilEngine::new();
        // A native sleep runs no opcodes, so no interrupt can reach it.
        engine.eval("(time/sleep-ms 60000)", "stuck");
        // Wait for the (lazily built) engine to pick the job up.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !engine.is_busy() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !engine.is_busy() {
            let o = block_on(outcomes.next()).unwrap();
            panic!("sleep finished early: {:?} / {:?}", o.value(), o.error());
        }
        // Reset abandons the wedged thread instead of queueing behind it…
        engine.reset();
        let started = std::time::Instant::now();
        engine.eval("(+ 1 2)", "fresh");
        let o = block_on(outcomes.next()).unwrap();
        // …so the next eval answers from a fresh image right away, and the
        // abandoned eval never reports.
        assert_eq!(o.callback(), "fresh");
        assert_eq!(o.value(), Some("3"));
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn captured_output_is_capped() {
        let (engine, mut outcomes) = AnvilEngine::new();
        engine.eval(
            "(let loop ((i 0)) (when (< i 40000) (display \"0123456789\") (loop (+ i 1))))",
            "cb",
        );
        let o = block_on(outcomes.next()).unwrap();
        let out = o.output().unwrap();
        assert!(
            out.len() < MAX_OUTPUT_BYTES + 100,
            "got {} bytes",
            out.len()
        );
        assert!(out.contains("more bytes of output dropped"));
    }
}
