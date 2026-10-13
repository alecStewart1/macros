//! Language primitives: bundled and installed tree-sitter grammars, and the
//! extension / language-id tables.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Languages);
    {
        // `(bundled-languages)` → every language id with a bundled tree-sitter
        // grammar. language-modes.scm reads this to generate an `<id>-mode` M-x
        // command per language (so `M-x scala-mode` switches the buffer's mode).
        let langs: Vec<String> = crate::highlight::BUNDLED_LANGUAGES
            .iter()
            .map(|s| s.to_string())
            .collect();
        reg.register_fn("bundled-languages", move || -> Vec<String> {
            langs.clone()
        });
    }
    {
        // `(installed-languages)` → ids of grammars downloaded + compiled at
        // runtime (M-x install-language). grammar.scm reads this alongside
        // `(bundled-languages)` so installed languages also get `<id>-mode`.
        reg.register_fn("installed-languages", || -> Vec<String> {
            crate::grammar::installed_ids()
        });
    }
    {
        // `(grammar-install id repo rev extensions symbol subdir)` → queue an
        // async download + compile + load of a tree-sitter grammar. `extensions`
        // is a whitespace-separated string of bare extensions; `symbol` and
        // `subdir` are "" for the defaults (parser symbol = id, grammar at repo
        // root). The Scheme `install-language` wrapper builds these from nicer
        // args. Returns immediately; progress/result echoes to the minibuffer
        // (see Editor::install_grammar).
        let q = queue.clone();
        reg.register_fn(
            "grammar-install",
            move |id: String,
                  repo: String,
                  rev: String,
                  extensions: String,
                  symbol: String,
                  subdir: String| {
                q.push(EditorCommand::InstallGrammar(Box::new(GrammarInstall {
                    id,
                    repo,
                    rev,
                    extensions,
                    symbol,
                    subdir,
                })));
            },
        );
    }
    register_binary(reg, queue, "set-extension", EditorCommand::SetExtension);
    register_binary(reg, queue, "set-language-id", EditorCommand::SetLanguageId);
}
