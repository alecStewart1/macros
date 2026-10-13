//! Key bindings and the command list: `define-key` plus its read-side shadow.

use std::sync::{Arc, Mutex};

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Keymaps);
    // Shadow of every binding (mode, chord, command), for help/discoverability
    // read bridges. Written alongside the DefineKey command; lives on the engine
    // thread with the read fns.
    let bindings: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let q = queue.clone();
        let shadow = bindings.clone();
        reg.register_fn(
            "define-key",
            move |mode: String, chord: String, command: Callback| {
                // The command may be a "name" string OR a 'name symbol (the
                // canonical config form). Both resolve to the same name; it's
                // looked up and run by name at keypress time.
                let Callback(command) = command;
                {
                    let mut s = shadow.lock().unwrap();
                    // Last definition for a (mode, chord) wins (matches the keymap).
                    s.retain(|(m, c, _)| !(m == &mode && c == &chord));
                    s.push((mode.clone(), chord.clone(), command.clone()));
                }
                q.push(EditorCommand::DefineKey(mode, chord, command));
            },
        );
    }
    {
        // `(key-binding mode chord)` → the command bound there, or "".
        let shadow = bindings.clone();
        reg.register_fn(
            "key-binding",
            move |mode: String, chord: String| -> String {
                shadow
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(m, c, _)| m == &mode && c == &chord)
                    .map(|(_, _, cmd)| cmd.clone())
                    .unwrap_or_default()
            },
        );
    }
    {
        // `(keys-with-prefix mode prefix)` → "chord\tcommand" lines whose chord
        // starts with `prefix` (which-key / describe-prefix).
        let shadow = bindings.clone();
        reg.register_fn(
            "keys-with-prefix",
            move |mode: String, prefix: String| -> Vec<String> {
                shadow
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(m, c, _)| m == &mode && c.starts_with(&prefix))
                    .map(|(_, c, cmd)| format!("{c}\t{cmd}"))
                    .collect()
            },
        );
    }
    {
        // `(command-list)` → all nullary command names (M-x / apropos).
        let names: Vec<String> = nullary_commands()
            .iter()
            .map(|(n, _)| n.to_string())
            .collect();
        reg.register_fn("command-list", move || -> Vec<String> { names.clone() });
    }
}
