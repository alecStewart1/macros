//! Tree-sitter primitives: synchronous syntax-tree reads over the published
//! snapshot, and async queries.

use steel::rvals::SteelVal;

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared {
        queue, snapshot, ..
    } = ctx;
    reg.section(Section::TreeSitter);
    // `(buffer-language)` → the highlighting language ("" when none).
    register_snapshot_read(reg, snapshot, "buffer-language", |s| s.language.clone());
    {
        // `(ts-ancestors pos)` → flat `(kind start end …)` of every syntax node
        // containing pos, innermost first; `()` without a tree. Synchronous —
        // the published snapshot carries the tree.
        let shared = snapshot.clone();
        reg.register_fn("ts-ancestors", move |pos: usize| -> Vec<SteelVal> {
            let snap = shared.load();
            let Some(tree) = snap.tree.as_ref() else {
                return Vec::new();
            };
            crate::highlight::tree_ancestors(tree, pos)
                .into_iter()
                .flat_map(|(k, s, e)| {
                    [
                        SteelVal::StringV(k.into()),
                        SteelVal::IntV(s as isize),
                        SteelVal::IntV(e as isize),
                    ]
                })
                .collect()
        });
    }
    {
        // `(ts-children start end)` → flat `(kind start end named? …)` of the
        // children of the node spanning exactly [start, end).
        let shared = snapshot.clone();
        reg.register_fn(
            "ts-children",
            move |start: usize, end: usize| -> Vec<SteelVal> {
                let snap = shared.load();
                let Some(tree) = snap.tree.as_ref() else {
                    return Vec::new();
                };
                crate::highlight::tree_children(tree, start, end)
                    .into_iter()
                    .flat_map(|(k, s, e, named)| {
                        [
                            SteelVal::StringV(k.into()),
                            SteelVal::IntV(s as isize),
                            SteelVal::IntV(e as isize),
                            SteelVal::BoolV(named),
                        ]
                    })
                    .collect()
            },
        );
    }
    {
        // `(ts-ranges-of-kinds '("function_item" …))` → flat `(start end …)` of
        // every node of those kinds, in document order.
        let shared = snapshot.clone();
        reg.register_fn(
            "ts-ranges-of-kinds",
            move |StringList(kinds): StringList| -> Vec<usize> {
                let snap = shared.load();
                let Some(tree) = snap.tree.as_ref() else {
                    return Vec::new();
                };
                crate::highlight::tree_ranges_of_kinds(tree, &kinds)
                    .into_iter()
                    .flat_map(|(s, e)| [s, e])
                    .collect()
            },
        );
    }
    register_binary(
        reg,
        queue,
        "tree-sitter-query",
        |query, callback: Callback| EditorCommand::TreeSitterQuery {
            query,
            callback: callback.0,
        },
    );
    register_binary(
        reg,
        queue,
        "tree-sitter-node-at",
        |byte, callback: Callback| EditorCommand::TreeSitterNodeAt {
            byte,
            callback: callback.0,
        },
    );
}
