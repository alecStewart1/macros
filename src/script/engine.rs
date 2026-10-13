use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use steel::rvals::{IntoSteelVal, SteelVal};

use super::*;

/// The outcome of a blocking eval: enqueued commands + the last value.
/// Only [`ScriptEngine::eval`] (tests / REPL) builds this; startup uses
/// [`ScriptEngine::eval_collecting`], which keeps commands even on error.
#[allow(dead_code)]
pub struct EvalResult {
    pub commands: Vec<EditorCommand>,
    pub value: Option<String>,
}

/// Something the engine thread sends back to the editor's foreground drain task.
pub enum ScriptOutcome {
    /// A dispatched eval/action: commands to apply + an optional echo message.
    Commands {
        commands: Vec<EditorCommand>,
        message: Option<String>,
    },
    /// Dynamic Helm candidates for `source`, computed for query `query_id`
    /// (stale results — id mismatch — are discarded by the editor). Each entry is
    /// a candidate plus an optional marginalia-style annotation.
    Candidates {
        source: usize,
        query_id: u64,
        candidates: Vec<(String, Option<String>)>,
    },
    /// Completion-at-point results from Scheme CAPFs: the byte range to replace
    /// and the candidate items (label + optional annotation). Empty = no CAPF
    /// produced anything, so the editor falls back to LSP.
    Capf {
        start: usize,
        end: usize,
        items: Vec<(String, Option<String>)>,
    },
}

pub(crate) struct RawResult {
    commands: Vec<EditorCommand>,
    value: Option<String>,
    error: Option<String>,
}

/// Where a job's result goes: back to a blocking caller, or to the async drain.
pub(crate) enum Reply {
    Sync(Sender<RawResult>),
    Async,
}

pub(crate) enum Task {
    Eval(String),
    Call(String, Option<String>),
    /// Like `Call`, but the argument is a secret (a `read-passwd` result): an
    /// error is reported WITHOUT Steel's message, which quotes the offending
    /// value verbatim (`+ expects a number, found: "hunter2"`) and would put
    /// it in the echo area and `*Messages*`.
    CallSecret(String, String),
    /// Compute dynamic Helm candidates: call `function` with the pattern.
    Candidates {
        source: usize,
        query_id: u64,
        function: String,
        pattern: String,
    },
    /// Run completion-at-point functions in order until one yields candidates.
    Capf {
        functions: Vec<String>,
        prefix: String,
        start: usize,
        end: usize,
    },
}

pub(crate) struct Job {
    task: Task,
    interactive: bool,
    reply: Reply,
}

/// A handle to the Steel interpreter, which runs on its OWN thread so a slow or
/// runaway script can never freeze the UI (the editor's command loop solves
/// Emacs's "elisp blocks the main loop" problem). The engine is `!Send`, so it
/// is created on and never leaves that thread — only plain data (`String`,
/// `EditorCommand`) crosses the channel boundary. The editor talks to it two
/// ways: **blocking** (startup/config, where command ordering matters, and
/// tests) and **dispatched** (interactive commands, drained on the foreground).
pub struct ScriptEngine {
    jobs: Sender<Job>,
    command_names: Vec<String>,
    /// Every name registered into the engine, recorded at registration time by
    /// [`Registrar`] on the engine thread (the single source of truth — it can
    /// never drift from what is actually registered). Registration happens on
    /// the spawned engine thread, so the list arrives on a one-shot channel and
    /// is cached here on first use.
    global_names: std::sync::OnceLock<Vec<String>>,
    global_names_rx: Mutex<Option<Receiver<Vec<String>>>>,
    /// Fast path: a nullary command name → its `EditorCommand`, so trivial
    /// keybindings (motion, etc.) apply synchronously without a thread hop.
    builtins: HashMap<String, EditorCommand>,
    buffers: BufferNames,
    buffer_paths: BufferPaths,
    snapshot: SharedSnapshot,
    markers: Markers,
    options: Options,
    session: SharedSession,
}

impl ScriptEngine {
    /// Spawn the engine thread; returns the handle and the receiver of async
    /// outcomes (the editor drains it on a foreground task).
    pub fn new() -> (Self, UnboundedReceiver<ScriptOutcome>) {
        let nullary = nullary_commands();
        let command_names: Vec<String> = nullary.iter().map(|(name, _)| name.to_string()).collect();
        let builtins: HashMap<String, EditorCommand> = nullary
            .iter()
            .map(|(n, c)| (n.to_string(), c.clone()))
            .collect();

        let buffers: BufferNames = Arc::new(Mutex::new(Vec::new()));
        let buffer_paths: BufferPaths = Arc::new(Mutex::new(HashMap::new()));
        let snapshot: SharedSnapshot = Arc::new(ArcSwap::from_pointee(EditorSnapshot::default()));
        let markers: Markers = Arc::new(Mutex::new(MarkerStore::default()));
        let options: Options = Arc::new(Mutex::new(HashMap::new()));
        let session: SharedSession = Arc::new(Mutex::new(SessionState::default()));
        let (jobs, jobs_rx) = channel::<Job>();
        let (outcomes, outcomes_rx) = unbounded::<ScriptOutcome>();
        let (names_tx, names_rx) = channel::<Vec<String>>();

        let thread_buffers = buffers.clone();
        let thread_buffer_paths = buffer_paths.clone();
        let thread_snapshot = snapshot.clone();
        let thread_markers = markers.clone();
        let thread_options = options.clone();
        let thread_session = session.clone();
        std::thread::spawn(move || {
            engine_thread(
                jobs_rx,
                outcomes,
                names_tx,
                thread_buffers,
                thread_buffer_paths,
                thread_snapshot,
                thread_markers,
                thread_options,
                thread_session,
            )
        });

        (
            Self {
                jobs,
                command_names,
                global_names: std::sync::OnceLock::new(),
                global_names_rx: Mutex::new(Some(names_rx)),
                builtins,
                buffers,
                buffer_paths,
                snapshot,
                markers,
                options,
                session,
            },
            outcomes_rx,
        )
    }

    /// Shift markers after an edit replacing `[start, old_end)` with `new_len`
    /// bytes: markers inside the deleted span collapse to `start`, markers after
    /// it shift by the length delta.
    pub fn adjust_markers(&self, start: usize, old_end: usize, new_len: usize) {
        let delta = new_len as isize - (old_end - start) as isize;
        let mut store = self.markers.lock().unwrap();
        for pos in store.positions.values_mut() {
            if *pos >= old_end {
                *pos = (*pos as isize + delta).max(0) as usize;
            } else if *pos > start {
                *pos = start;
            }
        }
    }

    /// Drop all markers (e.g. on a buffer switch — markers are buffer-positions).
    pub fn clear_markers(&self) {
        let mut store = self.markers.lock().unwrap();
        store.positions.clear();
    }

    /// Create a marker at `pos`, returning its id. The snippet session uses these
    /// to track tab-stop fields, which then ride along on `adjust_markers`.
    pub fn add_marker(&self, pos: usize) -> u64 {
        self.markers.lock().unwrap().alloc(pos)
    }

    /// Current byte position of marker `id`, if it still exists.
    pub fn marker_pos(&self, id: u64) -> Option<usize> {
        self.markers.lock().unwrap().positions.get(&id).copied()
    }

    /// Remove marker `id` (snippet field cleanup).
    pub fn remove_marker(&self, id: u64) {
        self.markers.lock().unwrap().positions.remove(&id);
    }

    /// The nullary command names, for `M-x` completion.
    pub fn command_names(&self) -> &[String] {
        &self.command_names
    }

    /// Every registered global name, for the built-in Steel LSP. Recorded on
    /// the engine thread as functions are registered; the first call waits for
    /// registration to finish (fast — it happens before any job is processed).
    pub fn global_names(&self) -> &[String] {
        self.global_names.get_or_init(|| {
            self.global_names_rx
                .lock()
                .unwrap()
                .take()
                .and_then(|rx| rx.recv().ok())
                .unwrap_or_default()
        })
    }

    /// Update the buffer list Scheme sees via `(buffer-names)`.
    pub fn set_buffer_names(&self, names: Vec<String>) {
        *self.buffers.lock().unwrap() = names;
    }

    /// Publish the buffer name → file path map Scheme reads via
    /// `(buffer-file-name-of name)`. Only file-visiting buffers appear.
    pub fn set_buffer_paths(&self, paths: HashMap<String, String>) {
        *self.buffer_paths.lock().unwrap() = paths;
    }

    /// Publish the current editor options so Scheme can read them via
    /// `(get-option "name")`. Called after any option change.
    pub fn set_options(&self, options: HashMap<String, crate::settings::OptionValue>) {
        *self.options.lock().unwrap() = options;
    }

    /// Publish the session-persistence state (rings, buffers, layout) that
    /// `(session-ring …)` / `(window-layout)` / `(session-file-buffers)` read.
    pub fn set_session(&self, state: SessionState) {
        *self.session.lock().unwrap() = state;
    }

    /// Publish the active buffer name and point line for `(current-buffer)` /
    /// `(buffer-line)`. Called just before dispatching so a command reads point
    /// as it was when invoked.
    pub fn set_snapshot(&self, snapshot: EditorSnapshot) {
        self.snapshot.store(Arc::new(snapshot));
    }

    /// Recognise one or more bare `(builtin)` forms — `(forward-char)` or a
    /// sequence like `(set-mark) (move-end-of-line)` — and return their commands
    /// in order, so a motion/edit keybinding applies synchronously instead of
    /// paying a thread hop. All-or-nothing: if any form takes an argument, is a
    /// string, nests, or names a non-builtin, returns `None` and the whole
    /// expression goes to the engine thread (where its semantics are exact).
    pub fn builtins_for(&self, expr: &str) -> Option<Vec<EditorCommand>> {
        let mut out = Vec::new();
        let mut rest = expr.trim();
        while !rest.is_empty() {
            // Each form is `(name)` with no nested parens, so the first `)`
            // closes it. Anything with a string/arg fails the char check below
            // and drops the whole expression to the engine.
            let inner = rest.strip_prefix('(')?;
            let close = inner.find(')')?;
            let name = inner[..close].trim();
            if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '(' || c == '"')
            {
                return None;
            }
            out.push(self.builtins.get(name)?.clone());
            rest = inner[close + 1..].trim();
        }
        (!out.is_empty()).then_some(out)
    }

    /// Send `task` to the engine thread and block for its raw result. Both
    /// failure points — the engine thread gone at send or at receive —
    /// collapse to the same "script engine stopped" error. The blocking
    /// public entry points ([`Self::eval`], [`Self::eval_collecting`],
    /// [`Self::call_action`]) are thin wrappers over this.
    fn run_sync(&self, task: Task) -> Result<RawResult, String> {
        let (tx, rx) = channel();
        self.jobs
            .send(Job {
                task,
                interactive: false,
                reply: Reply::Sync(tx),
            })
            .map_err(|_| "script engine stopped".to_string())?;
        rx.recv().map_err(|_| "script engine stopped".to_string())
    }

    /// Blocking eval — tests and the REPL. Startup config uses
    /// [`Self::eval_collecting`] (which keeps partial commands on error).
    #[allow(dead_code)]
    pub fn eval(&self, source: &str) -> Result<EvalResult, String> {
        let raw = self.run_sync(Task::Eval(source.to_string()))?;
        match raw.error {
            Some(error) => Err(error),
            None => Ok(EvalResult {
                commands: raw.commands,
                value: raw.value,
            }),
        }
    }

    /// Like [`Self::eval`] but never throws the commands away on error: returns
    /// everything queued *before* the failure together with the error message.
    /// Startup config uses this so one bad form in `init.scm` can't silently drop
    /// the rest of the file — including the `(evil-mode)` that usually precedes
    /// it (Emacs `init.el` semantics: apply up to the error, then report it).
    pub fn eval_collecting(&self, source: &str) -> (Vec<EditorCommand>, Option<String>) {
        match self.run_sync(Task::Eval(source.to_string())) {
            Ok(raw) => (raw.commands, raw.error),
            Err(stopped) => (Vec::new(), Some(stopped)),
        }
    }

    /// Blocking action call — used by tests; the editor uses `dispatch_action`.
    #[allow(dead_code)]
    pub fn call_action(&self, name: &str, candidate: Option<&str>) -> Vec<EditorCommand> {
        self.run_sync(Task::Call(name.to_string(), candidate.map(str::to_string)))
            .map(|raw| raw.commands)
            .unwrap_or_default()
    }

    /// Async eval — runs on the engine thread; the outcome is delivered to the
    /// foreground drain task. A slow script never blocks the UI.
    pub fn dispatch(&self, source: &str, interactive: bool) {
        let _ = self.jobs.send(Job {
            task: Task::Eval(source.to_string()),
            interactive,
            reply: Reply::Async,
        });
    }

    /// Async action call (Helm actions, mode hooks).
    pub fn dispatch_action(&self, name: &str, candidate: Option<&str>) {
        let _ = self.jobs.send(Job {
            task: Task::Call(name.to_string(), candidate.map(str::to_string)),
            interactive: false,
            reply: Reply::Async,
        });
    }

    /// Async call of `name` with a secret argument (see [`Task::CallSecret`]).
    pub fn dispatch_secret_action(&self, name: &str, secret: &str) {
        let _ = self.jobs.send(Job {
            task: Task::CallSecret(name.to_string(), secret.to_string()),
            interactive: false,
            reply: Reply::Async,
        });
    }

    /// A cheap `Send + Clone` handle that can deliver `dispatch_action` calls
    /// from another thread — for work that must leave the UI thread entirely
    /// (the spell-check worker) yet still hand its result to a named Scheme
    /// callback.
    pub fn action_handle(&self) -> ActionHandle {
        ActionHandle {
            jobs: self.jobs.clone(),
        }
    }

    /// Compute a dynamic Helm source's candidates on the engine thread; the
    /// result arrives as `ScriptOutcome::Candidates` for `query_id`.
    pub fn dispatch_candidates(&self, source: usize, query_id: u64, function: &str, pattern: &str) {
        let _ = self.jobs.send(Job {
            task: Task::Candidates {
                source,
                query_id,
                function: function.to_string(),
                pattern: pattern.to_string(),
            },
            interactive: false,
            reply: Reply::Async,
        });
    }

    /// Run the registered completion-at-point functions on the engine thread,
    /// over the prefix `buffer[start..end]`. The result arrives as
    /// `ScriptOutcome::Capf`; empty means the editor should fall back to LSP.
    pub fn dispatch_capf(&self, functions: Vec<String>, prefix: &str, start: usize, end: usize) {
        let _ = self.jobs.send(Job {
            task: Task::Capf {
                functions,
                prefix: prefix.to_string(),
                start,
                end,
            },
            interactive: false,
            reply: Reply::Async,
        });
    }
}

/// A clone of the engine's job sender: delivers async action calls from any
/// thread. See [`ScriptEngine::action_handle`].
#[derive(Clone)]
pub struct ActionHandle {
    jobs: Sender<Job>,
}

impl ActionHandle {
    /// Blocking eval from any thread (the MCP server's): returns the editor
    /// commands queued — including those queued before an error — plus the
    /// printed value and the error, if any. The caller applies the commands
    /// on the editor's foreground; nothing here touches editor state.
    pub fn eval_sync(&self, source: &str) -> (Vec<EditorCommand>, Option<String>, Option<String>) {
        let (tx, rx) = channel();
        let sent = self.jobs.send(Job {
            task: Task::Eval(source.to_string()),
            interactive: false,
            reply: Reply::Sync(tx),
        });
        match sent.ok().and_then(|_| rx.recv().ok()) {
            Some(raw) => (raw.commands, raw.value, raw.error),
            None => (Vec::new(), None, Some("script engine stopped".to_string())),
        }
    }

    /// Async action call, identical to [`ScriptEngine::dispatch_action`].
    pub fn dispatch_action(&self, name: &str, candidate: Option<&str>) {
        let _ = self.jobs.send(Job {
            task: Task::Call(name.to_string(), candidate.map(str::to_string)),
            interactive: false,
            reply: Reply::Async,
        });
    }
}

/// The engine thread: owns the `!Send` `Engine` for its whole life, processing
/// jobs FIFO so results stay ordered.
#[allow(clippy::too_many_arguments)]
pub(crate) fn engine_thread(
    jobs: Receiver<Job>,
    outcomes: UnboundedSender<ScriptOutcome>,
    names_tx: Sender<Vec<String>>,
    buffers: BufferNames,
    buffer_paths: BufferPaths,
    snapshot: SharedSnapshot,
    markers: Markers,
    options: Options,
    session: SharedSession,
) {
    let queue: Queue = Arc::new(Mutex::new(Vec::new()));
    let (mut engine, names) = build_engine(
        &queue,
        &buffers,
        &buffer_paths,
        &snapshot,
        &markers,
        &options,
        &session,
    );
    let _ = names_tx.send(names);

    while let Ok(Job {
        task,
        interactive,
        reply,
    }) = jobs.recv()
    {
        // `with-process-env` extents never span jobs (one an error escaped
        // without popping would otherwise leak into every later spawn).
        crate::process::reset_dynamic();
        let raw = match task {
            // Dynamic Helm candidates: call the fn, parse a string list, reply
            // directly on the async channel (no command/value semantics).
            Task::Candidates {
                source,
                query_id,
                function,
                pattern,
            } => {
                let arg = pattern.into_steelval().unwrap_or(SteelVal::Void);
                let candidates = engine
                    .call_function_by_name_with_args(&function, vec![arg])
                    .ok()
                    .map(|value| steel_candidate_list(&value))
                    .unwrap_or_default();
                // Candidates fns are queries, not commands: any side effects a
                // dynamic source queued (e.g. a stray `message`) are discarded
                // here rather than leaking into — and being attributed to — the
                // next eval's command batch.
                drop(drain(&queue));
                let _ = outcomes.unbounded_send(ScriptOutcome::Candidates {
                    source,
                    query_id,
                    candidates,
                });
                continue;
            }
            // Completion-at-point: call each registered CAPF with the prefix; the
            // first one returning a non-empty candidate list wins. A CAPF returns
            // either a flat candidate list, or `(start end (cand …))` to override
            // the replaced byte range. Reply on the async channel.
            Task::Capf {
                functions,
                prefix,
                start,
                end,
            } => {
                let mut out_start = start;
                let mut out_end = end;
                let mut items: Vec<(String, Option<String>)> = Vec::new();
                for function in &functions {
                    let arg = prefix.clone().into_steelval().unwrap_or(SteelVal::Void);
                    let Ok(value) = engine.call_function_by_name_with_args(function, vec![arg])
                    else {
                        continue;
                    };
                    let (s, e, parsed) = parse_capf_result(&value, start, end);
                    if !parsed.is_empty() {
                        out_start = s;
                        out_end = e;
                        items = parsed;
                        break;
                    }
                }
                // CAPFs are queries, not commands: discard anything they queued
                // so it can't leak into the next eval's command batch.
                drop(drain(&queue));
                let _ = outcomes.unbounded_send(ScriptOutcome::Capf {
                    start: out_start,
                    end: out_end,
                    items,
                });
                continue;
            }
            Task::Eval(source) => match engine.run(source) {
                Ok(values) => RawResult {
                    commands: drain(&queue),
                    value: values.last().and_then(format_value),
                    error: None,
                },
                Err(error) => RawResult {
                    commands: drain(&queue),
                    value: None,
                    error: Some(steel_error_string(&error)),
                },
            },
            Task::Call(name, arg) => {
                let args: Vec<SteelVal> = arg
                    .and_then(|c| c.into_steelval().ok())
                    .into_iter()
                    .collect();
                let error = engine
                    .call_function_by_name_with_args(&name, args)
                    .err()
                    .map(|e| steel_error_string(&e));
                RawResult {
                    commands: drain(&queue),
                    value: None,
                    error,
                }
            }
            Task::CallSecret(name, secret) => {
                let args: Vec<SteelVal> = secret.into_steelval().into_iter().collect();
                let error = engine
                    .call_function_by_name_with_args(&name, args)
                    .err()
                    .map(|_| format!("{name} failed (details hidden: its argument is secret)"));
                RawResult {
                    commands: drain(&queue),
                    value: None,
                    error,
                }
            }
        };

        match reply {
            Reply::Sync(tx) => {
                let _ = tx.send(raw);
            }
            Reply::Async => {
                let RawResult {
                    commands,
                    value,
                    error,
                } = raw;
                let message = if let Some(error) = error {
                    Some(format!("error: {error}"))
                } else if interactive {
                    match value {
                        Some(v) => Some(format!("=> {v}")),
                        // A `(message ...)` side-effect already echoed something;
                        // don't clobber it with a generic "Done".
                        None if commands
                            .iter()
                            .any(|c| matches!(c, EditorCommand::Message(_))) =>
                        {
                            None
                        }
                        None => Some("Done".to_string()),
                    }
                } else {
                    None
                };
                let _ = outcomes.unbounded_send(ScriptOutcome::Commands { commands, message });
            }
        }
    }
}

pub(crate) fn drain(queue: &Queue) -> Vec<EditorCommand> {
    queue.lock().unwrap().drain(..).collect()
}
