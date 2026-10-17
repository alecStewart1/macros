//! Subprocess primitives (sync and streaming), the process environment, and
//! embedded terminals.

use crate::util::HideConsole;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    let async_ids = &ctx.ids;
    reg.section(Section::Processes);
    // `(executable-find name)` → the full path of `name` on `PATH`, or "" when
    // it isn't installed (Emacs `executable-find`).
    reg.register_fn("executable-find", |name: String| -> String {
        crate::util::executable_find(&name)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    // `(call-process program args)` — run a subprocess, return stdout. Blocks on
    // the engine thread (never the UI), the same model as `list-directory`.
    reg.register_fn(
        "call-process",
        move |program: String, StringList(args): StringList| -> String {
            let program = crate::util::expand_tilde(&program);
            let mut cmd = std::process::Command::new(&program);
            cmd.args(&args).hide_console();
            crate::process::current().apply(&mut cmd);
            cmd.output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        },
    );
    // `(call-process-full program args input)` → `(exit-code stdout stderr)`,
    // writing `input` to stdin (empty = none). Used for `git apply -` (hunk
    // staging) and to surface git errors (stderr + non-zero code).
    //
    // Every spawner (call-process*, make-process*, make-terminal, dap-start)
    // also applies the `with-process-env` overrides in effect — see
    // `crate::process`.
    reg.register_fn(
        "call-process-full",
        move |program: String, StringList(args): StringList, input: String| -> Vec<String> {
            let env = crate::process::current();
            crate::process::call_full(&program, &args, input, &env)
        },
    );
    // `(call-process-env program args input env)` — `call-process-full` with
    // extra environment for the child only. `env` is a list of "KEY=VALUE"
    // strings layered over the inherited environment; a bare "KEY" removes
    // the variable. The way to hand a subprocess a secret (PGPASSWORD, …)
    // without putting it in argv, where `ps` shows it to every local user.
    reg.register_fn(
        "call-process-env",
        move |program: String,
              StringList(args): StringList,
              input: String,
              StringList(env): StringList|
              -> Vec<String> {
            let env = crate::process::current_then(&env);
            crate::process::call_full(&program, &args, input, &env)
        },
    );
    // `(process-env-push! env)` / `(process-env-pop!)` — the primitives under
    // core.scm's `(with-process-env env thunk)`: every process spawned while
    // an extent is open gets `env` (same list shape as `call-process-env`).
    // The engine clears the stack before each job, so an extent left by an
    // error never outlives it.
    reg.register_fn("process-env-push!", |StringList(env): StringList| {
        crate::process::push_dynamic(crate::process::EnvOverrides::parse(&env));
    });
    reg.register_fn("process-env-pop!", crate::process::pop_dynamic);
    // `(getenv name)` → the variable's value, or "" if unset. Sees `setenv`
    // and any open `with-process-env` extent — what a child spawned now gets.
    reg.register_fn("getenv", |name: String| -> String {
        crate::process::getenv(&name).unwrap_or_default()
    });
    // `(setenv name value)` → set an env var for every subsequently spawned
    // subprocess (LSP servers, call-process, make-process, terminals). Lets a
    // config extend PATH or pin a toolchain, e.g.
    //   (setenv "PATH" (string-append (getenv "PATH") ":" ...))
    // Recorded in an overlay applied at spawn time, never via
    // `std::env::set_var`: other threads read the environment concurrently.
    reg.register_fn("setenv", |name: String, value: String| {
        crate::process::set_global(&name, &value);
    });
    {
        // `(make-process program args on-output on-exit)` → a process id (used by
        // process-send-string / kill-process). on-exit "" = none; it receives the
        // exit code as a string.
        // `(make-process-env program args on-output on-exit env)` — the same,
        // with `env` ("KEY=VALUE" / bare "KEY" = unset) for the child only (see
        // `call-process-env`). Both capture the `with-process-env` overrides
        // NOW: the spawn itself happens later, on the UI thread.
        let make = |ids: IdGen, q: Queue| {
            move |program: String,
                  StringList(args): StringList,
                  on_output: Callback,
                  on_exit: Callback,
                  env: crate::process::EnvOverrides|
                  -> usize {
                let id = ids.next();
                q.push(EditorCommand::MakeProcess(Box::new(MakeProcessArgs {
                    id,
                    program,
                    args,
                    env,
                    on_output: on_output.0,
                    on_exit: Some(on_exit.0).filter(|s| !s.is_empty()),
                })));
                id as usize
            }
        };
        let spawn = make(async_ids.clone(), queue.clone());
        reg.register_fn(
            "make-process",
            move |program: String, args: StringList, on_output: Callback, on_exit: Callback| {
                spawn(program, args, on_output, on_exit, crate::process::current())
            },
        );
        let spawn = make(async_ids.clone(), queue.clone());
        reg.register_fn(
            "make-process-env",
            move |program: String,
                  args: StringList,
                  on_output: Callback,
                  on_exit: Callback,
                  StringList(env): StringList| {
                let env = crate::process::current_then(&env);
                spawn(program, args, on_output, on_exit, env)
            },
        );
    }
    register_binary(reg, queue, "process-send-string", |id, text| {
        EditorCommand::SendProcess { id, text }
    });
    register_unary(reg, queue, "kill-process", |id| {
        EditorCommand::KillProcess { id }
    });
    // `(set-process-buffer id name)`: killing buffer `name` terminates process
    // `id` (its whole process group), like Emacs's process buffer.
    register_binary(reg, queue, "set-process-buffer", |id, name| {
        EditorCommand::SetProcessBuffer { id, name }
    });

    reg.section(Section::Terminal);
    {
        // `(make-terminal name program args directory)` → a terminal id. Empty
        // `program` runs the user's login shell; empty `directory` uses the
        // active buffer's directory. The terminal is shown in a `term-mode`
        // buffer named `name`; switching to that buffer later reuses it.
        let q = queue.clone();
        let ids = async_ids.clone();
        reg.register_fn(
            "make-terminal",
            move |name: String,
                  program: String,
                  StringList(args): StringList,
                  directory: String|
                  -> usize {
                let id = ids.next();
                q.push(EditorCommand::MakeTerminal(Box::new(MakeTerminalArgs {
                    id,
                    name,
                    program,
                    args,
                    directory,
                    env: crate::process::current(),
                })));
                id as usize
            },
        );
    }
}
