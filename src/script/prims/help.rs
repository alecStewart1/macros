//! Introspection primitives: the doc index behind describe-function /
//! apropos, the package-API reference, and the editor version.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use steel::rvals::SteelVal;

use super::*;

pub(in crate::script) fn register(
    reg: &mut Registrar,
    _ctx: &EngineShared,
    api_cell: &Arc<OnceLock<Vec<ApiEntry>>>,
) {
    reg.section(Section::Help);
    {
        // The doc index: name → (doc, file, line). Steel has no runtime
        // docstrings, so the `;;` block preceding each top-level define IS its
        // documentation; file+line let the picker jump to the implementation.
        // Seeded at startup from the bundled library and grown at runtime as
        // packages load (`index-doc-file`), so built-ins and dynamically loaded
        // packages are searched uniformly. Keyed by name → an UPSERT (a
        // redefinition or a package reload overwrites the entry), so the index
        // stays flat no matter how often a file is reloaded in a session.
        //
        // Powers `(function-doc name)`, `(function-list)`, `(function-source
        // name)` → describe-function / apropos / helm-apropos.
        type DocIndex = HashMap<String, (String, String, usize)>;
        let docs: Arc<RwLock<DocIndex>> = Arc::new(RwLock::new(
            crate::runtime::builtin_docs()
                .into_iter()
                .map(|e| (e.name, (e.doc, e.file, e.line)))
                .collect(),
        ));

        // `(function-doc name)` → the doc comment, or "".
        let d = docs.clone();
        reg.register_fn("function-doc", move |name: String| -> String {
            d.read()
                .unwrap()
                .get(&name)
                .map(|(doc, _, _)| doc.clone())
                .unwrap_or_default()
        });

        // `(api-reference category search)` → the package-API reference as
        // text: "" "" for the category index, a category ("sockets",
        // "lib/core") for its signatures and docs, search words to filter. The
        // MCP `api` tool is this.
        let cell = api_cell.clone();
        reg.register_fn(
            "api-reference",
            move |category: String, search: String| -> String {
                render_api(
                    cell.get().map(Vec::as_slice).unwrap_or(&[]),
                    &category,
                    &search,
                )
            },
        );

        // `(function-list)` → every indexed name, sorted.
        let d = docs.clone();
        reg.register_fn("function-list", move || -> Vec<String> {
            let mut names: Vec<String> = d.read().unwrap().keys().cloned().collect();
            names.sort();
            names
        });

        // `(function-source name)` → `(file line)` (1-based line), or `'()` when
        // the name is unknown or carries no location. The jump action opens
        // `file` at `line`, or falls back to the doc when the file is absent.
        let d = docs.clone();
        reg.register_fn("function-source", move |name: String| -> Vec<SteelVal> {
            match d.read().unwrap().get(&name) {
                Some((_, file, line)) if !file.is_empty() => vec![
                    SteelVal::StringV(file.clone().into()),
                    SteelVal::IntV(*line as isize),
                ],
                _ => vec![],
            }
        });

        // `(index-doc-file path)` → scrape `path` from disk and merge its
        // documented defines into the index (upsert by name). Called as a package
        // loads so its commented functions/variables become searchable and
        // jumpable. Returns the number of defines indexed (0 if unreadable).
        let d = docs.clone();
        reg.register_fn("index-doc-file", move |path: String| -> usize {
            let Ok(src) = std::fs::read_to_string(&path) else {
                return 0;
            };
            let entries = crate::runtime::scrape_docs(&src, &path);
            let n = entries.len();
            let mut idx = d.write().unwrap();
            for e in entries {
                idx.insert(e.name, (e.doc, e.file, e.line));
            }
            n
        });
    }
    {
        // `(macros-version)` → the editor's package version (from Cargo.toml,
        // baked in at compile time). Powers the `version` command.
        reg.register_fn("macros-version", || -> String {
            env!("CARGO_PKG_VERSION").to_string()
        });
    }
}
