//! Session-persistence primitives for scheme/session.scm (savehist + desktop):
//! read the history rings / file buffers / window layout the editor publishes
//! on demand ([`SessionState`]), and queue the commands that restore them.
//! Kept in one place (rather than spread through `registry.rs`) so the whole
//! surface is easy to find and review.

use std::sync::atomic::{AtomicBool, Ordering};

use steel::rvals::SteelVal;

use super::*;

/// Set once some window in this process has claimed the desktop (see
/// `session-claim-primary!`): only the first window restores and autosaves the
/// desktop, so `cmd-n` doesn't reopen every file in the new window.
static PRIMARY_CLAIMED: AtomicBool = AtomicBool::new(false);

/// A Steel symbol value.
fn sym(s: &str) -> SteelVal {
    SteelVal::SymbolV(s.to_string().into())
}

/// A Steel string value.
fn string(s: &str) -> SteelVal {
    SteelVal::StringV(s.to_string().into())
}

/// Render a [`LayoutSpec`] as the nested-list form Scheme sees:
/// `(leaf name path point selected?)` / `(split vertical? ratio a b)`.
pub(crate) fn layout_to_steel(spec: &LayoutSpec) -> SteelVal {
    match spec {
        LayoutSpec::Leaf {
            buffer,
            path,
            point,
            selected,
        } => SteelVal::ListV(
            vec![
                sym("leaf"),
                string(buffer),
                string(path),
                SteelVal::IntV(*point as isize),
                SteelVal::BoolV(*selected),
            ]
            .into(),
        ),
        LayoutSpec::Split {
            vertical,
            ratio,
            a,
            b,
        } => SteelVal::ListV(
            vec![
                sym("split"),
                SteelVal::BoolV(*vertical),
                // Round through the f32's shortest decimal so 0.3 reads back as
                // 0.3, not 0.30000001192092896.
                SteelVal::NumV(ratio.to_string().parse::<f64>().unwrap_or(0.5)),
                layout_to_steel(a),
                layout_to_steel(b),
            ]
            .into(),
        ),
    }
}

/// A non-negative integer from a Steel number (ints, or floats truncated).
fn steel_usize(v: &SteelVal) -> Option<usize> {
    match v {
        SteelVal::IntV(i) if *i >= 0 => Some(*i as usize),
        SteelVal::NumV(f) if *f >= 0.0 => Some(*f as usize),
        _ => None,
    }
}

/// A float from a Steel number.
fn steel_f32(v: &SteelVal) -> Option<f32> {
    match v {
        SteelVal::IntV(i) => Some(*i as f32),
        SteelVal::NumV(f) => Some(*f as f32),
        _ => None,
    }
}

/// Truthiness the Scheme way: only `#f` is false.
fn truthy(v: &SteelVal) -> bool {
    !matches!(v, SteelVal::BoolV(false))
}

/// Parse the nested-list layout form back into a [`LayoutSpec`] (the inverse of
/// [`layout_to_steel`]). The head may be a symbol or a string. `None` on any
/// malformed node, so a corrupt desktop file restores nothing rather than half
/// a tree.
pub(crate) fn steel_to_layout(value: &SteelVal) -> Option<LayoutSpec> {
    let items = steel_list(value)?;
    match steel_string(items.first()?)?.as_str() {
        "leaf" => Some(LayoutSpec::Leaf {
            buffer: steel_string(items.get(1)?)?,
            path: items.get(2).and_then(steel_string).unwrap_or_default(),
            point: items.get(3).and_then(steel_usize).unwrap_or(0),
            selected: items.get(4).is_some_and(truthy),
        }),
        "split" => Some(LayoutSpec::Split {
            vertical: truthy(items.get(1)?),
            ratio: steel_f32(items.get(2)?)?.clamp(0.1, 0.9),
            a: Box::new(steel_to_layout(items.get(3)?)?),
            b: Box::new(steel_to_layout(items.get(4)?)?),
        }),
        _ => None,
    }
}

/// `((path point) …)` (or dotted `(path . point)` pairs) → `(path, point)`s.
fn steel_path_points(value: &SteelVal) -> Vec<(String, usize)> {
    steel_list(value)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| match entry {
            SteelVal::Pair(pair) => Some((
                steel_string(pair.car_ref())?,
                steel_usize(pair.cdr_ref()).unwrap_or(0),
            )),
            other => {
                let parts = steel_list(other)?;
                Some((
                    steel_string(parts.first()?)?,
                    parts.get(1).and_then(steel_usize).unwrap_or(0),
                ))
            }
        })
        .collect()
}

/// The non-flag command-line arguments (files the editor was launched with).
fn command_line_files() -> Vec<String> {
    std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect()
}

/// Register the session primitives. Reads go through `session` (published by
/// the editor on `session-refresh` / before `kill-emacs-hook`); writes queue
/// [`EditorCommand`]s.
pub(crate) fn register_session(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, session, .. } = ctx;
    // (session-refresh 'callback): publish the rings/buffers/layout, then call
    // `callback` (no args) — the only way to read them fresh from a timer.
    register_unary(reg, queue, "session-refresh", |cb: Callback| {
        EditorCommand::SessionRefresh(cb.0)
    });
    {
        // (session-ring "kill") → the ring's entries, oldest first. Rings:
        // "minibuffer", "kill", "search", "command"; anything else is '().
        let shared = session.clone();
        reg.register_fn("session-ring", move |ring: SteelVal| -> Vec<String> {
            let state = shared.lock().unwrap();
            match steel_string(&ring).unwrap_or_default().as_str() {
                "minibuffer" => state.minibuffer_history.clone(),
                "kill" => state.kill_ring.clone(),
                "search" => state.search_ring.clone(),
                "command" => state.command_history.clone(),
                _ => Vec::new(),
            }
        });
    }
    {
        // (set-session-ring! "kill" items): replace a ring wholesale.
        let q = queue.clone();
        reg.register_fn(
            "set-session-ring!",
            move |ring: SteelVal, StringList(items): StringList| {
                q.push(EditorCommand::SetSessionRing {
                    ring: steel_string(&ring).unwrap_or_default(),
                    items,
                });
            },
        );
    }
    {
        // (session-file-buffers) → ((path point) …), buffer-ring order with the
        // active buffer last.
        let shared = session.clone();
        reg.register_fn("session-file-buffers", move || -> SteelVal {
            let state = shared.lock().unwrap();
            SteelVal::ListV(
                state
                    .file_buffers
                    .iter()
                    .map(|(path, point)| {
                        SteelVal::ListV(vec![string(path), SteelVal::IntV(*point as isize)].into())
                    })
                    .collect(),
            )
        });
    }
    {
        // (window-layout) → the split tree, or #f if never published.
        let shared = session.clone();
        reg.register_fn("window-layout", move || -> SteelVal {
            match &shared.lock().unwrap().layout {
                Some(spec) => layout_to_steel(spec),
                None => SteelVal::BoolV(false),
            }
        });
    }
    {
        // (set-window-layout tree) → #t if the tree parsed (and was queued).
        let q = queue.clone();
        reg.register_fn("set-window-layout", move |tree: SteelVal| -> bool {
            match steel_to_layout(&tree) {
                Some(spec) => {
                    q.push(EditorCommand::SetWindowLayout(spec));
                    true
                }
                None => false,
            }
        });
    }
    {
        // (restore-file-buffers '((path point) …)): visit files in the
        // background without switching; missing files are skipped.
        let q = queue.clone();
        reg.register_fn("restore-file-buffers", move |entries: SteelVal| {
            q.push(EditorCommand::RestoreFileBuffers(steel_path_points(
                &entries,
            )));
        });
    }
    // (command-line-files) → files passed on the command line (desktop restore
    // is skipped when the editor was launched on explicit files).
    reg.register_fn("command-line-files", command_line_files);
    // (session-claim-primary!) → #t for the first caller in this process only.
    reg.register_fn("session-claim-primary!", || -> bool {
        !PRIMARY_CLAIMED.swap(true, Ordering::SeqCst)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_round_trips_through_steel_values() {
        let spec = LayoutSpec::Split {
            vertical: false,
            ratio: 0.3,
            a: Box::new(LayoutSpec::Leaf {
                buffer: "main.rs".into(),
                path: "/p/src/main.rs".into(),
                point: 42,
                selected: true,
            }),
            b: Box::new(LayoutSpec::Split {
                vertical: true,
                ratio: 0.5,
                a: Box::new(LayoutSpec::Leaf {
                    buffer: "*scratch*".into(),
                    path: String::new(),
                    point: 0,
                    selected: false,
                }),
                b: Box::new(LayoutSpec::Leaf {
                    buffer: "lib.rs".into(),
                    path: "/p/src/lib.rs".into(),
                    point: 7,
                    selected: false,
                }),
            }),
        };
        let back = steel_to_layout(&layout_to_steel(&spec)).expect("parses");
        assert_eq!(back, spec);
    }

    #[test]
    fn malformed_layout_is_rejected_and_ratio_clamped() {
        // Unknown head.
        assert!(steel_to_layout(&SteelVal::ListV(vec![sym("tree")].into())).is_none());
        // Split missing a child.
        let half =
            SteelVal::ListV(vec![sym("split"), SteelVal::BoolV(true), SteelVal::NumV(0.5)].into());
        assert!(steel_to_layout(&half).is_none());
        // Out-of-range ratio clamps; a string head works too.
        let leaf = SteelVal::ListV(vec![string("leaf"), string("a")].into());
        let wide = SteelVal::ListV(
            vec![
                sym("split"),
                SteelVal::BoolV(false),
                SteelVal::NumV(5.0),
                leaf.clone(),
                leaf,
            ]
            .into(),
        );
        match steel_to_layout(&wide) {
            Some(LayoutSpec::Split { ratio, .. }) => assert!((ratio - 0.9).abs() < 1e-6),
            other => panic!("expected split, got {other:?}"),
        }
    }
}
