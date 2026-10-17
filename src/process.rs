//! Subprocess plumbing shared by every spawner (call-process, make-process,
//! terminals, DAP adapters, language servers): environment overrides and
//! [`ProcessGroup`], the one owner of process-group signalling.
//!
//! Scheme hands a child extra environment two ways:
//! - explicitly, as the `env` list of `call-process-env` / `make-process-env`;
//! - dynamically, via `(with-process-env env thunk)` (Emacs's
//!   `process-environment` let-binding): every process spawned inside the
//!   thunk's extent picks the overrides up, whichever primitive spawned it.
//!
//! A third, process-wide layer sits under both: `(setenv KEY VALUE)` records
//! into [`set_global`] instead of mutating the real process environment
//! (`std::env::set_var` is unsound once other threads exist, and the UI,
//! executor, LSP readers and fs watcher all do). Every spawn applies it first.
//!
//! The dynamic stack is thread-local to the Steel engine thread (where every
//! spawning primitive runs or, for async spawns, *captures* its env before the
//! command is queued). The engine resets it at the start of each job, so a
//! thunk that errors out of `with-process-env` can't leak overrides into the
//! next job.

use std::cell::RefCell;
use std::process::{Child, ExitStatus};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Child-process env overrides: `(KEY, Some(value))` sets, `(KEY, None)`
/// unsets. Applied in order on top of the inherited environment, so a later
/// entry for the same key wins.
///
/// `Debug` prints keys only: values are routinely secrets (PGPASSWORD, API
/// tokens), and `EditorCommand` derives `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvOverrides(pub Vec<(String, Option<String>)>);

impl std::fmt::Debug for EnvOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(k, v)| match v {
                Some(_) => format!("{k}=***"),
                None => format!("-{k}"),
            }))
            .finish()
    }
}

impl EnvOverrides {
    /// `self` followed by `later` (whose entries win on a shared key).
    pub fn then(mut self, later: EnvOverrides) -> EnvOverrides {
        self.0.extend(later.0);
        self
    }

    /// Parse a Scheme env list: "KEY=VALUE" sets (split at the FIRST `=`, so a
    /// value may contain `=`); a bare "KEY" unsets. Empty keys are dropped.
    pub fn parse(items: &[String]) -> EnvOverrides {
        EnvOverrides(
            items
                .iter()
                .filter_map(|item| match item.split_once('=') {
                    Some((k, v)) if !k.is_empty() => Some((k.to_string(), Some(v.to_string()))),
                    None if !item.is_empty() => Some((item.clone(), None)),
                    _ => None,
                })
                .collect(),
        )
    }

    /// Apply to a `Command`, on top of the environment it inherits and the
    /// global `setenv` overlay.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        apply_global(cmd);
        for (k, v) in &self.0 {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
    }

    /// Fold into a PTY's set-only env map. A PTY child inherits our process
    /// env and the map can only add to it, so an unset becomes an empty value
    /// (the closest a set-only map gets; `${X-default}` will still see it as
    /// set).
    pub fn apply_to_map(&self, map: &mut std::collections::HashMap<String, String>) {
        for (k, v) in global_snapshot() {
            map.insert(k, v);
        }
        for (k, v) in &self.0 {
            map.insert(k.clone(), v.clone().unwrap_or_default());
        }
    }
}

/// The `setenv` overlay: process-wide, insertion-ordered, one entry per key.
static GLOBAL: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

fn global_snapshot() -> Vec<(String, String)> {
    GLOBAL.read().map(|g| g.clone()).unwrap_or_default()
}

/// `(setenv key value)`: set `key` for every process spawned from now on.
pub fn set_global(key: &str, value: &str) {
    let Ok(mut g) = GLOBAL.write() else { return };
    match g.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value.to_string(),
        None => g.push((key.to_string(), value.to_string())),
    }
}

/// Apply only the global `setenv` overlay — for spawners that take no
/// Scheme overrides (grammar compiles, gpg). Keys the command already sets
/// explicitly keep their value, as they would over a real inherited env.
pub fn apply_global(cmd: &mut std::process::Command) {
    let explicit: Vec<_> = cmd.get_envs().map(|(k, _)| k.to_owned()).collect();
    for (k, v) in global_snapshot() {
        if !explicit.iter().any(|e| *e == *k) {
            cmd.env(k, v);
        }
    }
}

/// `(getenv key)`: what a child spawned right now would see — the innermost
/// `with-process-env` override, then the `setenv` overlay, then the real
/// environment. `None` when unset (or unset by an override).
pub fn getenv(key: &str) -> Option<String> {
    let dynamic = DYNAMIC.with(|d| {
        d.borrow().iter().rev().find_map(|e| {
            e.0.iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        })
    });
    if let Some(v) = dynamic {
        return v;
    }
    if let Some(v) = GLOBAL
        .read()
        .ok()
        .and_then(|g| g.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    {
        return Some(v);
    }
    std::env::var(key).ok()
}

thread_local! {
    /// The `with-process-env` stack (innermost last).
    static DYNAMIC: RefCell<Vec<EnvOverrides>> = const { RefCell::new(Vec::new()) };
}

/// Enter a `with-process-env` extent.
pub fn push_dynamic(env: EnvOverrides) {
    DYNAMIC.with(|d| d.borrow_mut().push(env));
}

/// Leave the innermost `with-process-env` extent (no-op when none is open).
pub fn pop_dynamic() {
    DYNAMIC.with(|d| {
        d.borrow_mut().pop();
    });
}

/// Drop every open extent — the engine calls this before each job.
pub fn reset_dynamic() {
    DYNAMIC.with(|d| d.borrow_mut().clear());
}

/// The overrides in effect right now (outermost first, so inner wins).
pub fn current() -> EnvOverrides {
    DYNAMIC.with(|d| {
        EnvOverrides(
            d.borrow()
                .iter()
                .flat_map(|e| e.0.iter().cloned())
                .collect(),
        )
    })
}

/// The dynamic overrides followed by an explicit `env` list (explicit wins).
pub fn current_then(items: &[String]) -> EnvOverrides {
    current().then(EnvOverrides::parse(items))
}

/// Wait for `child` and collect its output, writing `input` to its stdin (then
/// closing it, so the child sees EOF — otherwise a filter like `git apply -`
/// blocks waiting for input). The write runs on its own thread: a child that
/// fills its stdout pipe before draining stdin would otherwise deadlock
/// against us while we're still blocked writing.
pub(crate) fn output_with_stdin(
    mut child: std::process::Child,
    input: Vec<u8>,
) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    let writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let output = child.wait_with_output();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    output
}

/// Run `program` to completion → `[exit-code, stdout, stderr]` (the Scheme
/// `call-process-full` shape), writing `input` to stdin (empty = none).
pub fn call_full(program: &str, args: &[String], input: String, env: &EnvOverrides) -> Vec<String> {
    use crate::util::HideConsole;
    use std::process::Stdio;
    let program = crate::util::expand_tilde(program);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .hide_console();
    env.apply(&mut cmd);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return vec!["-1".into(), String::new(), e.to_string()],
    };
    match output_with_stdin(child, input.into_bytes()) {
        Ok(o) => vec![
            o.status
                .code()
                .map_or_else(|| "-1".into(), |c| c.to_string()),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ],
        Err(e) => vec!["-1".into(), String::new(), e.to_string()],
    }
}

/// How often [`ProcessGroup`]'s reap loops poll `try_wait`.
const REAP_POLL: Duration = Duration::from_millis(20);

/// A signal [`ProcessGroup::signal`] can send to a whole group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Ask politely (SIGTERM).
    Term,
    /// Force (SIGKILL).
    Kill,
}

#[cfg(unix)]
impl Signal {
    fn raw(self) -> i32 {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// A child spawned as the leader of its own process group
/// (`Command::process_group(0)`), so signalling it takes down whatever a
/// wrapper (`npx`, `sh -c`, `opam exec`) spawned too, not just the wrapper.
///
/// Tracks whether the child has been reaped: until then its pid (= the pgid)
/// can't be recycled, so signalling the group is sound; afterwards it must
/// never be signalled again. This is the one place that `kill`s a group.
///
/// Dropping an unreaped group SIGKILLs it and reaps the zombie (promptly,
/// since it was just killed — but it does block, so a UI-thread owner should
/// [`kill_now`](Self::kill_now) and drop it on another thread).
///
/// Off unix there are no process groups: signals fall back to killing the
/// child itself.
pub struct ProcessGroup {
    child: Child,
    reaped: bool,
}

/// One poll loop's outcome (see [`poll_exit`]).
enum Polled {
    Exited(ExitStatus),
    Failed,
    TimedOut,
}

/// Poll `try_wait` every [`REAP_POLL`] until the child exits, it errors, or
/// `deadline` (if any) passes.
fn poll_exit(
    mut try_wait: impl FnMut() -> std::io::Result<Option<ExitStatus>>,
    deadline: Option<Instant>,
) -> Polled {
    loop {
        match try_wait() {
            Ok(Some(status)) => return Polled::Exited(status),
            Ok(None) if deadline.is_none_or(|d| Instant::now() < d) => {
                std::thread::sleep(REAP_POLL);
            }
            Ok(None) => return Polled::TimedOut,
            Err(_) => return Polled::Failed,
        }
    }
}

impl ProcessGroup {
    /// Wrap a child spawned with `process_group(0)`. Take its pipes first.
    pub fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    /// Poll for exit without blocking; a returned status means it is reaped.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    /// Signal the group if the leader is still running (checked with
    /// `try_wait` first, which reaps an exited leader); true if it was.
    pub fn signal(&mut self, #[allow(unused)] sig: Signal) -> bool {
        if !matches!(self.try_wait(), Ok(None)) {
            return false;
        }
        #[cfg(unix)]
        // SAFETY: plain FFI call. The leader is unreaped, so the pgid is ours.
        unsafe {
            libc::kill(-(self.child.id() as i32), sig.raw());
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        true
    }

    /// SIGKILL the group (and the leader directly) unless already reaped.
    /// Unlike [`signal`](Self::signal) this doesn't poll first: a leader that
    /// exited but isn't reaped still holds the pgid, and whatever it left
    /// running in the group must die too.
    pub fn kill_now(&mut self) {
        if self.reaped {
            return;
        }
        #[cfg(unix)]
        // SAFETY: plain FFI call. The leader is unreaped, so the pgid is ours.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
    }

    /// Wait up to `grace` for the leader to exit on its own, then SIGKILL the
    /// group and reap it. BLOCKING — run it off the UI thread. `None` if
    /// waiting failed.
    pub fn reap_with_grace(&mut self, grace: Duration) -> Option<ExitStatus> {
        let child = &mut self.child;
        let status = match poll_exit(|| child.try_wait(), Some(Instant::now() + grace)) {
            Polled::Exited(status) => Some(status),
            Polled::Failed => None,
            Polled::TimedOut => {
                self.kill_now();
                self.child.wait().ok()
            }
        };
        self.reaped = true;
        status
    }

    /// Poll a group shared with signallers until it exits, releasing the lock
    /// between polls so they can get in. BLOCKING — run it on a worker.
    /// `None` if waiting failed.
    pub fn wait_shared(group: &Mutex<ProcessGroup>) -> Option<ExitStatus> {
        let lock = || group.lock().unwrap_or_else(|e| e.into_inner());
        match poll_exit(|| lock().try_wait(), None) {
            Polled::Exited(status) => Some(status),
            Polled::Failed | Polled::TimedOut => None,
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill_now();
            // SIGKILLed above, so this returns promptly; it reaps the zombie.
            let _ = self.child.wait();
        }
    }
}

/// The lines of `reader`, like [`BufRead::lines`] (a trailing `\n` or `\r\n`
/// stripped) but decoding each line lossily: a process or HTTP stream that
/// emits one non-UTF-8 line (a Latin-1 filename, a binary blob) keeps flowing
/// instead of `lines()` erroring and the reader dropping the pipe. Ends at EOF
/// or on a real I/O error.
pub fn lossy_lines<R: std::io::BufRead>(mut reader: R) -> impl Iterator<Item = String> {
    let mut buf = Vec::new();
    std::iter::from_fn(move || {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_shapes() {
        assert_eq!(
            EnvOverrides::parse(&strings(&["A=1", "B=x=y", "C=", "D", "=bad", ""])).0,
            vec![
                ("A".into(), Some("1".into())),
                ("B".into(), Some("x=y".into())),
                ("C".into(), Some(String::new())),
                ("D".into(), None),
            ]
        );
    }

    #[test]
    fn debug_hides_values() {
        let env = EnvOverrides::parse(&strings(&["PGPASSWORD=hunter2", "HOME"]));
        let shown = format!("{env:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(
            shown.contains("PGPASSWORD=***") && shown.contains("-HOME"),
            "{shown}"
        );
    }

    #[test]
    fn dynamic_stack_nests_and_resets() {
        reset_dynamic();
        push_dynamic(EnvOverrides::parse(&strings(&["A=outer", "B=1"])));
        push_dynamic(EnvOverrides::parse(&strings(&["A=inner"])));
        let mut cmd = std::process::Command::new("true");
        current_then(&strings(&["C=x"])).apply(&mut cmd);
        let envs: Vec<_> = cmd.get_envs().collect();
        // Inner A wins over outer A; explicit C rides along.
        assert!(envs.contains(&(
            std::ffi::OsStr::new("A"),
            Some(std::ffi::OsStr::new("inner"))
        )));
        assert!(envs.contains(&(std::ffi::OsStr::new("B"), Some(std::ffi::OsStr::new("1")))));
        assert!(envs.contains(&(std::ffi::OsStr::new("C"), Some(std::ffi::OsStr::new("x")))));
        pop_dynamic();
        assert_eq!(
            current().0,
            vec![
                ("A".into(), Some("outer".into())),
                ("B".into(), Some("1".into()))
            ]
        );
        reset_dynamic();
        assert!(current().0.is_empty());
        pop_dynamic(); // popping an empty stack is harmless
    }

    /// The env reaches a real child; the editor's own env is untouched.
    #[cfg(unix)]
    #[test]
    fn call_full_sets_and_unsets_child_env() {
        let env = EnvOverrides::parse(&strings(&["MACROS_PT=a=b c", "HOME"]));
        let r = call_full(
            "/bin/sh",
            &strings(&["-c", "printf '%s|%s' \"$MACROS_PT\" \"${HOME-unset}\""]),
            String::new(),
            &env,
        );
        assert_eq!(r[0], "0");
        assert_eq!(r[1], "a=b c|unset");
        assert!(std::env::var("MACROS_PT").is_err());
        assert!(std::env::var("HOME").is_ok());
    }
}

#[cfg(test)]
mod lossy_lines_tests {
    use super::lossy_lines;

    #[test]
    fn non_utf8_line_does_not_end_the_stream() {
        let input: &[u8] = b"one\r\nbad \xff\xfe byte\nthree";
        let lines: Vec<String> = lossy_lines(input).collect();
        assert_eq!(lines, vec!["one", "bad \u{fffd}\u{fffd} byte", "three"]);
    }
}
