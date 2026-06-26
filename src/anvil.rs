//! ANVIL — an interactive Steel REPL backed by a *second*, fully independent
//! Steel interpreter.
//!
//! The editor already embeds one `Engine` (its config/scripting engine, see
//! [`crate::script`]). ANVIL spins up a separate `Engine` on its own thread so
//! REPL experiments live in their own image: user `define`s never pollute the
//! config engine, the image can be restarted from scratch, and — crucially — a
//! runaway evaluation can be *interrupted* without taking the editor down.
//!
//! Interruption is cooperative but free: Steel's VM checks an atomic interrupt
//! flag on every opcode (`ThreadStateController`). So any pure-Scheme infinite
//! loop or unbounded recursion — `(define (spin) (spin))` included — unwinds
//! with an error at its next instruction once [`AnvilEngine::interrupt`] flips
//! the flag from the UI thread. (The one case it can't reach is code stuck
//! inside a blocking Rust host call, which isn't executing opcodes — but the
//! REPL engine registers no such host bridges.)
//!
//! The plumbing mirrors [`crate::script::ScriptEngine`]: the `!Send` engine is
//! built on and never leaves its thread; only plain data crosses the channels.
//! Jobs are delivered over a blocking `std::sync::mpsc` channel (the thread
//! `recv()`s), and results come back over a `futures` channel the editor drains
//! on its foreground task.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use steel::rvals::SteelVal;
use steel::steel_vm::ThreadStateController;
use steel::steel_vm::engine::Engine;

use crate::script::format_value;

/// Something the ANVIL engine thread sends back to the editor's foreground drain.
pub enum AnvilOutcome {
    /// A finished eval or introspection query, to be rendered by a Scheme
    /// callback in the *main* engine.
    Result {
        /// The callback that should render this result into the REPL buffer.
        callback: String,
        /// The printed value, or `None` for void/no value.
        value: Option<String>,
        /// Error text if the eval failed — including `"Interrupted by user"`
        /// when the evaluation was cancelled.
        error: Option<String>,
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
    /// Throw away the current image and build a fresh one.
    Reset,
}

/// A handle to the ANVIL REPL interpreter. Cheap to hold; all the state lives on
/// the engine thread.
pub struct AnvilEngine {
    jobs: Sender<AnvilJob>,
    /// The running engine's interrupt controller. The UI thread calls
    /// `interrupt()` here to cancel an in-flight eval; the engine thread swaps
    /// it on [`AnvilJob::Reset`] (a rebuilt engine has a fresh controller).
    controller: Arc<Mutex<ThreadStateController>>,
}

impl AnvilEngine {
    /// Spawn the REPL engine thread; returns the handle and the receiver of
    /// async outcomes (the editor drains it on a foreground task).
    pub fn new() -> (Self, UnboundedReceiver<AnvilOutcome>) {
        let (jobs, jobs_rx) = channel::<AnvilJob>();
        let (outcomes, outcomes_rx) = unbounded::<AnvilOutcome>();
        // A placeholder until the thread installs the real engine's controller.
        let controller = Arc::new(Mutex::new(ThreadStateController::default()));
        let thread_controller = controller.clone();
        // The engine thread signals readiness once it has built its `Engine` and
        // published the real interrupt controller. We block on it here so the
        // placeholder is never observable to callers: otherwise an `interrupt()`
        // arriving before the (sometimes slow) `Engine::new()` finishes would
        // flip the placeholder's flag instead of the VM's and be silently lost,
        // leaving a runaway eval spinning forever.
        let (ready_tx, ready_rx) = channel::<()>();
        std::thread::spawn(move || anvil_thread(jobs_rx, outcomes, thread_controller, ready_tx));
        let _ = ready_rx.recv();
        (Self { jobs, controller }, outcomes_rx)
    }

    /// Queue a REPL evaluation. The printed result is delivered to `callback`
    /// (a function in the main engine) via the outcome channel.
    pub fn eval(&self, source: &str, callback: &str) {
        let _ = self.jobs.send(AnvilJob::Eval {
            source: source.to_string(),
            callback: callback.to_string(),
        });
    }

    /// Queue an image-introspection query; the rendered result is delivered to
    /// `callback` (a function in the main engine) via the outcome channel.
    pub fn query(&self, kind: AnvilQuery, callback: &str) {
        let _ = self.jobs.send(AnvilJob::Query {
            kind,
            callback: callback.to_string(),
        });
    }

    /// Queue an image-backed completion: the live globals starting with
    /// `prefix`, to replace the buffer range `start..end`. The result arrives as
    /// [`AnvilOutcome::Completions`] for the editor's completion popup.
    pub fn complete(&self, prefix: &str, start: usize, end: usize) {
        let _ = self.jobs.send(AnvilJob::Complete {
            prefix: prefix.to_string(),
            start,
            end,
        });
    }

    /// Cancel the current evaluation (SLIME-style `C-c C-c`). Sets the VM's
    /// interrupt flag; the running eval unwinds at its next opcode. Safe to call
    /// when nothing is running — the next eval clears the flag first.
    pub fn interrupt(&self) {
        self.controller.lock().unwrap().interrupt();
    }

    /// Rebuild the interpreter from scratch — a fresh image with no user
    /// definitions (SLIME's restart-inferior-lisp). Interrupts any in-flight
    /// eval first so the queued rebuild can actually be reached even if the REPL
    /// is wedged in a loop.
    pub fn reset(&self) {
        self.controller.lock().unwrap().interrupt();
        let _ = self.jobs.send(AnvilJob::Reset);
    }
}

/// The REPL engine thread: owns the `!Send` `Engine` for its whole life,
/// processing jobs FIFO.
fn anvil_thread(
    jobs: std::sync::mpsc::Receiver<AnvilJob>,
    outcomes: UnboundedSender<AnvilOutcome>,
    controller: Arc<Mutex<ThreadStateController>>,
    ready: Sender<()>,
) {
    let mut engine = Engine::new();
    *controller.lock().unwrap() = engine.get_thread_state_controller();
    // Real controller is in place — let `new()` return now that an interrupt
    // can actually reach the VM.
    let _ = ready.send(());

    while let Ok(job) = jobs.recv() {
        match job {
            AnvilJob::Eval { source, callback } => {
                // Clear any leftover interrupt (e.g. a `reset()` that targeted a
                // now-finished eval) so this fresh run starts cleanly.
                controller.lock().unwrap().resume();
                // Redirect this eval's output to a fresh in-image string port so
                // `(display …)` lands in the REPL, not the process stdout. Run as
                // its own top-level form *before* the user's source so the user's
                // `define`s stay top-level (wrapping them in a `let` wouldn't).
                let _ = engine.run("(current-output-port (open-output-string))");
                let (value, error) = match engine.run(source) {
                    Ok(values) => (values.last().and_then(format_value), None),
                    Err(e) => (None, Some(e.to_string())),
                };
                // The run may have ended via interrupt, which leaves the VM in
                // the Interrupted state — reset it so the next eval isn't
                // immediately poisoned.
                controller.lock().unwrap().resume();
                let output = drain_output(&mut engine);
                let _ = outcomes.unbounded_send(AnvilOutcome::Result {
                    callback,
                    value,
                    error,
                    output,
                });
            }
            AnvilJob::Query { kind, callback } => {
                controller.lock().unwrap().resume();
                let value = Some(run_query(&mut engine, kind));
                let _ = outcomes.unbounded_send(AnvilOutcome::Result {
                    callback,
                    value,
                    error: None,
                    output: None,
                });
            }
            AnvilJob::Complete { prefix, start, end } => {
                let items = complete_globals(&engine, &prefix);
                let _ = outcomes.unbounded_send(AnvilOutcome::Completions { start, end, items });
            }
            AnvilJob::Reset => {
                engine = Engine::new();
                *controller.lock().unwrap() = engine.get_thread_state_controller();
            }
        }
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
        SteelVal::StringV(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

/// The live image's global bindings whose name starts with `prefix`, sorted and
/// de-duplicated — the candidate set for SLIME-style symbol completion. Shares
/// `readable_globals` with apropos, so it sees everything the user has defined
/// this session, immediately.
fn complete_globals(engine: &Engine, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = engine
        .readable_globals(0)
        .iter()
        .map(|sym| sym.resolve().to_string())
        .filter(|name| name.starts_with(prefix))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Answer an [`AnvilQuery`] against the live image, rendered as display text.
fn run_query(engine: &mut Engine, kind: AnvilQuery) -> String {
    match kind {
        AnvilQuery::Apropos(pattern) => {
            let mut names: Vec<String> = engine
                .readable_globals(0)
                .iter()
                .map(|sym| sym.resolve().to_string())
                .filter(|name| name.contains(&pattern))
                .collect();
            names.sort();
            names.dedup();
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
                Err(e) => format!(";; macroexpand error: {e}"),
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
            AnvilOutcome::Result { value, .. } => value.as_deref(),
            _ => None,
        }
    }
    fn error(&self) -> Option<&str> {
        match self {
            AnvilOutcome::Result { error, .. } => error.as_deref(),
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
}
