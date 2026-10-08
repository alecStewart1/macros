//! Subprocess environment plumbing shared by every spawner (call-process,
//! make-process, terminals, DAP adapters).
//!
//! Scheme hands a child extra environment two ways:
//! - explicitly, as the `env` list of `call-process-env` / `make-process-env`;
//! - dynamically, via `(with-process-env env thunk)` (Emacs's
//!   `process-environment` let-binding): every process spawned inside the
//!   thunk's extent picks the overrides up, whichever primitive spawned it.
//!
//! The dynamic stack is thread-local to the Steel engine thread (where every
//! spawning primitive runs or, for async spawns, *captures* its env before the
//! command is queued). The engine resets it at the start of each job, so a
//! thunk that errors out of `with-process-env` can't leak overrides into the
//! next job.

use std::cell::RefCell;

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

    /// Apply to a `Command`, on top of the environment it inherits.
    pub fn apply(&self, cmd: &mut std::process::Command) {
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
        for (k, v) in &self.0 {
            map.insert(k.clone(), v.clone().unwrap_or_default());
        }
    }
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

/// Run `program` to completion → `[exit-code, stdout, stderr]` (the Scheme
/// `call-process-full` shape), writing `input` to stdin (empty = none).
pub fn call_full(program: &str, args: &[String], input: String, env: &EnvOverrides) -> Vec<String> {
    use std::io::Write;
    use std::process::Stdio;
    let program = crate::util::expand_tilde(program);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.apply(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return vec!["-1".into(), String::new(), e.to_string()],
    };
    // Write stdin on its own thread, then close it (drop) so the child sees
    // EOF — otherwise a filter like `git apply -` blocks waiting for input.
    // Not inline: a child that writes more than a pipe buffer of output before
    // draining its input would deadlock against us while we're still blocked
    // writing.
    let writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        })
    });
    let output = child.wait_with_output();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    match output {
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
