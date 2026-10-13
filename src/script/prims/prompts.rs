//! Prompt primitives: messages, minibuffer reads, Helm pickers, y-or-n-p and
//! key capture.

use steel::rvals::SteelVal;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Prompts);
    // `message` accepts any value, not just strings: strings render verbatim,
    // everything else via its Steel display — so (message (+ 1 2)) shows "3"
    // rather than raising a ConversionError.
    {
        let q = queue.clone();
        reg.register_fn("message", move |arg: SteelVal| {
            let text = match arg {
                SteelVal::StringV(s) => s.to_string(),
                other => other.to_string(),
            };
            q.push(EditorCommand::Message(text));
        });
    }
    {
        // `(helm sources)` where sources is a list of `(name candidates action)`.
        let q = queue.clone();
        reg.register_fn("helm", move |sources: SteelVal| {
            q.push(EditorCommand::Helm(parse_helm_sources(&sources)));
        });
        // `(helm-from initial sources)` — Helm with a pre-filled input.
        let q = queue.clone();
        reg.register_fn("helm-from", move |initial: String, sources: SteelVal| {
            q.push(EditorCommand::HelmFrom(
                initial,
                parse_helm_sources(&sources),
            ));
        });
    }
    {
        // Backing primitive for the `completing-read` Scheme wrapper (which makes
        // `initial` optional). `candidates` may be a static list OR a Scheme fn
        // name (string/symbol) — re-queried per keystroke with the current input,
        // the minibuffer equivalent of a dynamic Helm source.
        let q = queue.clone();
        reg.register_fn(
            "completing-read-impl",
            move |prompt: String, candidates: SteelVal, callback: Callback, initial: String| {
                let callback = callback.0;
                let (candidates, dynamic) = match steel_string(&candidates) {
                    Some(func) => (Vec::new(), Some(func)),
                    None => (steel_string_list(&candidates), None),
                };
                q.push(EditorCommand::CompletingRead {
                    prompt,
                    candidates,
                    dynamic,
                    initial,
                    callback,
                });
            },
        );
    }
    register_binary(
        reg,
        queue,
        "read-file-name",
        |prompt, callback: Callback| EditorCommand::ReadFileName {
            prompt,
            callback: callback.0,
        },
    );
    // `(y-or-n-p prompt callback)`; the `-sticky` variant can't be aborted
    // (C-g/Escape) — it stays in the echo area until answered, for prompts that
    // must resolve a pending request (a dropped read can't strand the requester).
    register_y_or_n_p(reg, queue, "y-or-n-p", false);
    register_y_or_n_p(reg, queue, "y-or-n-p-sticky", true);
    // Backing primitive for the `read-string` Scheme wrapper (which makes the
    // initial value optional). Callback is a Steel fn run with the entered text.
    register_ternary(
        reg,
        queue,
        "read-string-impl",
        |prompt, callback: Callback, initial| EditorCommand::ReadString {
            prompt,
            initial,
            callback: callback.0,
        },
    );
    // Backing primitive for `read-passwd`: a masked minibuffer read that never
    // enters history; `callback` gets the raw (untrimmed) input, `on-cancel`
    // ("" = none) runs with "" if the read is aborted or displaced.
    register_ternary(
        reg,
        queue,
        "read-passwd-impl",
        |prompt, callback: Callback, on_cancel: Callback| EditorCommand::ReadPasswd {
            prompt,
            callback: callback.0,
            on_cancel: Some(on_cancel.0).filter(|s| !s.is_empty()),
        },
    );
    // `(read-char "prompt" 'callback)` — capture the very next printable
    // keystroke and call `(callback "<char>")`. Vim's f/t/F/T char-find (and
    // anything else that reads one char) is built on this; `r` predates it and
    // keeps its dedicated EvilReplaceChar path.
    register_binary(reg, queue, "read-char", |prompt, callback: Callback| {
        EditorCommand::ReadChar {
            prompt,
            callback: callback.0,
        }
    });
    // `(read-key-start "prompt" 'callback)` — a STICKY capture: every key is
    // passed to `(callback "<key>")` (printable keys as the character, others
    // as their chord name: "enter", "backspace", "escape", "ctrl-g", …) until
    // `(read-key-stop)`. Escape / C-g also disarm. Backs query-replace.
    register_binary(
        reg,
        queue,
        "read-key-start",
        |prompt, callback: Callback| EditorCommand::ReadKeyStart {
            prompt,
            callback: callback.0,
        },
    );
    register_nullary(reg, queue, "read-key-stop", EditorCommand::ReadKeyStop);
}

/// Register one of the `y-or-n-p` / `y-or-n-p-sticky` pair: a yes/no prompt
/// calling `callback` with "yes"/"no"; `sticky` prompts can't be aborted.
pub(crate) fn register_y_or_n_p(
    reg: &mut Registrar,
    queue: &Queue,
    name: &'static str,
    sticky: bool,
) {
    let q = queue.clone();
    reg.register_fn(name, move |prompt: String, callback: Callback| {
        q.push(EditorCommand::YesOrNo {
            prompt,
            callback: callback.0,
            sticky,
        });
    });
}
