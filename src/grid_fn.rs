//! The interpreter that runs Scheme functions called from grid formulas
//! (`define-grid-function`).
//!
//! It is a *separate* Steel engine on its own thread — the same arrangement
//! as the ANVIL REPL ([`crate::anvil`]) — for two reasons. A grid function
//! may take a long time (a network lookup), and running it on the config
//! engine would freeze every Scheme-backed key binding until it finished.
//! And only a separate engine can be *interrupted*: Steel checks an atomic
//! interrupt flag on every opcode, so the watchdog thread here can stop a
//! call that outlives `grid-function-timeout` without touching the editor.
//!
//! Functions arrive as source text (each `define-grid-function` /
//! `define-grid-helper` form, printed by the config engine), so this engine
//! only knows what was explicitly shared with it — the reason grid functions
//! see nothing but their arguments.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use steel::rvals::SteelVal;
use steel::steel_vm::ThreadStateController;
use steel::steel_vm::engine::Engine;

use crate::formula::value::{ErrorKind, Value};
use crate::script::steel_error_string;

/// The default `grid-function-timeout`, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

enum Job {
    /// Evaluate a definition (a function or helper) in the image.
    Define { source: String },
    /// Call `expr` (a complete call expression) and report under `id`.
    Call { id: u64, expr: String },
}

/// What the interpreter thread reports back.
pub enum GridFnOutcome {
    /// A definition failed to evaluate.
    DefineFailed { error: String },
    /// A call finished: its value, or an error message (`timed_out` when the
    /// watchdog stopped it).
    Called {
        id: u64,
        result: Result<Value, String>,
        timed_out: bool,
    },
}

/// Handle to the grid-function interpreter. All state lives on its thread.
pub struct GridFnEngine {
    jobs: Sender<Job>,
    timeout_secs: Arc<AtomicU64>,
}

impl GridFnEngine {
    pub fn new() -> (Self, UnboundedReceiver<GridFnOutcome>) {
        let (jobs, jobs_rx) = channel::<Job>();
        let (outcomes, outcomes_rx) = unbounded::<GridFnOutcome>();
        let timeout_secs = Arc::new(AtomicU64::new(DEFAULT_TIMEOUT_SECS));
        let controller = Arc::new(Mutex::new(ThreadStateController::default()));
        // `(started, interrupted-by-watchdog)` of the call running now.
        let running: Arc<Mutex<Option<(Instant, bool)>>> = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = channel::<()>();
        {
            let controller = controller.clone();
            let running = running.clone();
            std::thread::spawn(move || run(jobs_rx, outcomes, controller, running, ready_tx));
        }
        let _ = ready_rx.recv();
        // The watchdog: interrupt a call that has run past the timeout.
        {
            let timeout = timeout_secs.clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    // The interpreter thread is gone (handle dropped): stop.
                    if Arc::strong_count(&running) == 1 {
                        break;
                    }
                    let limit = Duration::from_secs(timeout.load(Ordering::Relaxed));
                    let mut slot = running.lock().unwrap();
                    if let Some((started, stopped)) = slot.as_mut()
                        && !*stopped
                        && started.elapsed() > limit
                    {
                        *stopped = true;
                        controller.lock().unwrap().interrupt();
                    }
                }
            });
        }
        (Self { jobs, timeout_secs }, outcomes_rx)
    }

    /// Set the per-call time limit.
    pub fn set_timeout(&self, secs: u64) {
        self.timeout_secs.store(secs.max(1), Ordering::Relaxed);
    }

    /// Share a definition with the interpreter.
    pub fn define(&self, source: String) {
        let _ = self.jobs.send(Job::Define { source });
    }

    /// Queue a call of `function` with `args`; the result comes back under
    /// `id`. Arguments are formula values or ranges (rows of values).
    pub fn call(&self, id: u64, function: &str, args: &[crate::formula::engine::ExternalArg]) {
        let mut expr = format!("({function}");
        for a in args {
            expr.push(' ');
            match a {
                crate::formula::engine::ExternalArg::Value(v) => expr.push_str(&literal(v)),
                crate::formula::engine::ExternalArg::Rows(rows) => {
                    expr.push_str("(list");
                    for row in rows {
                        expr.push_str(" (list");
                        for v in row {
                            expr.push(' ');
                            expr.push_str(&literal(v));
                        }
                        expr.push(')');
                    }
                    expr.push(')');
                }
            }
        }
        expr.push(')');
        let _ = self.jobs.send(Job::Call { id, expr });
    }
}

/// A formula value as Scheme source: numbers, strings, `#t`/`#f`; empty is
/// `""`; an error is its text (`"#N/A"`).
fn literal(v: &Value) -> String {
    match v {
        Value::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{}", *n as i64),
        Value::Number(n) => format!("{n:?}"),
        Value::Text(s) => quote(s),
        Value::Bool(b) => (if *b { "#t" } else { "#f" }).to_string(),
        Value::Error(e) => quote(e.as_str()),
        Value::Empty => "\"\"".to_string(),
    }
}

use crate::script::steel_quote as quote;

/// A Scheme result as a cell value. A list (or anything else structured)
/// is `#VALUE!`: a cell holds one value (no spilling).
fn to_value(v: &SteelVal) -> Value {
    match v {
        SteelVal::IntV(i) => Value::Number(*i as f64),
        SteelVal::NumV(n) if n.is_finite() => Value::Number(*n),
        SteelVal::NumV(_) => Value::Error(ErrorKind::Num),
        SteelVal::Rational(r) => Value::Number(*r.numer() as f64 / *r.denom() as f64),
        SteelVal::StringV(s) => Value::text(s),
        SteelVal::SymbolV(s) => Value::text(s),
        SteelVal::CharV(c) => Value::text(&c.to_string()),
        SteelVal::BoolV(b) => Value::Bool(*b),
        SteelVal::Void => Value::Empty,
        _ => Value::Error(ErrorKind::Value),
    }
}

fn run(
    jobs: Receiver<Job>,
    outcomes: UnboundedSender<GridFnOutcome>,
    controller: Arc<Mutex<ThreadStateController>>,
    running: Arc<Mutex<Option<(Instant, bool)>>>,
    ready: Sender<()>,
) {
    let mut engine = Engine::new();
    *controller.lock().unwrap() = engine.get_thread_state_controller();
    let _ = ready.send(());
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Define { source } => {
                controller.lock().unwrap().resume();
                if let Err(e) = engine.run(source) {
                    let _ = outcomes.unbounded_send(GridFnOutcome::DefineFailed {
                        error: steel_error_string(&e),
                    });
                }
            }
            Job::Call { id, expr } => {
                controller.lock().unwrap().resume();
                *running.lock().unwrap() = Some((Instant::now(), false));
                let result = engine.run(expr);
                let timed_out = running
                    .lock()
                    .unwrap()
                    .take()
                    .is_some_and(|(_, stopped)| stopped);
                controller.lock().unwrap().resume();
                let result = match result {
                    Ok(vals) => Ok(vals.last().map_or(Value::Empty, to_value)),
                    Err(e) => Err(steel_error_string(&e)),
                };
                let _ = outcomes.unbounded_send(GridFnOutcome::Called {
                    id,
                    result,
                    timed_out,
                });
            }
        }
    }
}

/// Print a Steel datum back as source text (`write` style: strings quoted),
/// so a definition read by the config engine can be shared with this one.
pub fn write_datum(v: &SteelVal) -> String {
    match v {
        SteelVal::StringV(s) => quote(s),
        SteelVal::BoolV(b) => (if *b { "#t" } else { "#f" }).to_string(),
        SteelVal::CharV(c) => match c {
            ' ' => "#\\space".into(),
            '\n' => "#\\newline".into(),
            c => format!("#\\{c}"),
        },
        SteelVal::ListV(items) => {
            let parts: Vec<String> = items.iter().map(write_datum).collect();
            format!("({})", parts.join(" "))
        }
        SteelVal::VectorV(items) => {
            let parts: Vec<String> = items.iter().map(write_datum).collect();
            format!("#({})", parts.join(" "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::engine::ExternalArg;

    fn wait(rx: &mut UnboundedReceiver<GridFnOutcome>) -> GridFnOutcome {
        let start = Instant::now();
        loop {
            if let Ok(o) = rx.try_recv() {
                return o;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "no outcome");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn calls_return_values_and_errors() {
        let (e, mut rx) = GridFnEngine::new();
        e.define("(define (MARGIN p c) (/ (- p c) p))".into());
        e.call(
            1,
            "MARGIN",
            &[
                ExternalArg::Value(Value::Number(10.0)),
                ExternalArg::Value(Value::Number(7.5)),
            ],
        );
        match wait(&mut rx) {
            GridFnOutcome::Called { id, result, .. } => {
                assert_eq!(id, 1);
                assert_eq!(result, Ok(Value::Number(0.25)));
            }
            _ => panic!("expected a call result"),
        }
        e.define("(define (TOTAL rows) (apply + (map (lambda (r) (apply + r)) rows)))".into());
        e.call(
            2,
            "TOTAL",
            &[ExternalArg::Rows(vec![
                vec![Value::Number(1.0), Value::Number(2.0)],
                vec![Value::Number(3.0), Value::Number(4.0)],
            ])],
        );
        assert!(matches!(
            wait(&mut rx),
            GridFnOutcome::Called { result: Ok(Value::Number(n)), .. } if n == 10.0
        ));
        e.call(3, "NOPE", &[]);
        assert!(matches!(
            wait(&mut rx),
            GridFnOutcome::Called {
                result: Err(_),
                timed_out: false,
                ..
            }
        ));
    }

    #[test]
    fn the_watchdog_stops_runaway_calls() {
        let (e, mut rx) = GridFnEngine::new();
        e.set_timeout(1);
        e.define("(define (SPIN) (SPIN))".into());
        e.call(1, "SPIN", &[]);
        assert!(matches!(
            wait(&mut rx),
            GridFnOutcome::Called {
                timed_out: true,
                result: Err(_),
                ..
            }
        ));
        // The interpreter survives and keeps working.
        e.define("(define (ONE) 1)".into());
        e.call(2, "ONE", &[]);
        assert!(matches!(
            wait(&mut rx),
            GridFnOutcome::Called { id: 2, result: Ok(Value::Number(n)), .. } if n == 1.0
        ));
    }

    #[test]
    fn literals_and_datums() {
        assert_eq!(literal(&Value::text("a\"b")), "\"a\\\"b\"");
        assert_eq!(literal(&Value::Number(3.0)), "3");
        assert_eq!(literal(&Value::Number(0.5)), "0.5");
        assert_eq!(literal(&Value::Error(ErrorKind::NA)), "\"#N/A\"");
    }
}
