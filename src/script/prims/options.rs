//! Editor option primitives: `set-option`, `get-option` and the known option
//! names.

use steel::rvals::SteelVal;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, options, .. } = ctx;
    reg.section(Section::Options);
    {
        // `set-option` accepts a number, boolean, or string; the value is coerced
        // to the option's declared type when applied (see `Settings::set`).
        let q = queue.clone();
        reg.register_fn("set-option", move |key: String, value: SteelVal| {
            q.push(EditorCommand::SetOption(key, option_from_steelval(&value)));
        });
    }
    {
        // Read-side bridge: `(get-option "name")` returns the option's current
        // value as a native bool / number / string, or #f if it isn't set.
        // An option set to a secret reference (`(secret-get host user)`)
        // reads as the secret once the store can answer, #f until then —
        // gate on `(option-ensure name k)` to prompt for it.
        let opts = options.clone();
        reg.register_fn("get-option", move |key: String| -> SteelVal {
            let value = opts.lock().unwrap().get(&key).cloned();
            match value {
                None => SteelVal::BoolV(false),
                Some(crate::settings::OptionValue::Secret(r)) => r
                    .resolve()
                    .map_or(SteelVal::BoolV(false), |s| SteelVal::StringV(s.into())),
                Some(v) => option_to_steelval(&v),
            }
        });
        // `(option-secret-ref "name")` → `(host user port)` when the option is
        // a secret reference, else #f.
        let opts = options.clone();
        reg.register_fn("option-secret-ref", move |key: String| -> SteelVal {
            match opts.lock().unwrap().get(&key) {
                Some(crate::settings::OptionValue::Secret(r)) => SteelVal::ListV(
                    [&r.host, &r.user, &r.port]
                        .iter()
                        .map(|s| SteelVal::StringV(s.as_str().into()))
                        .collect(),
                ),
                _ => SteelVal::BoolV(false),
            }
        });
    }
    // `(option-list)` → every option key Rust knows about, so helm-apropos can
    // surface options ("font" → font-size) next to functions and variables.
    reg.register_fn("option-list", || -> Vec<String> {
        crate::settings::KNOWN_OPTIONS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
}
