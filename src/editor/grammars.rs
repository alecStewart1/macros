//! Tree-sitter grammar installs (with first-trust confirmation) and the
//! app auto-update flow for [`Editor`], extracted from the editor module for
//! size. These methods extend the same `Editor` type via an additional
//! `impl` block; cross-module entry points are `pub(super)`.

use gpui::Context;

use crate::highlight::Highlighter;

use super::*;

impl Editor {
    /// `M-x check-for-updates`: force a check against macros.dev, then echo the
    /// outcome. If a newer release exists, install it (macOS: download, verify,
    /// and swap in place; elsewhere: open the downloads page). All network/IO
    /// runs on the background executor; only echoes touch the editor. Unlike the
    /// launch-time check this never gates on the once-a-day interval.
    pub(super) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.echo("Checking for updates…", cx);
        let task = cx
            .background_executor()
            .spawn(async { crate::update::check() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |editor, cx| match result {
                Ok(None) => editor.echo(
                    format!(
                        "Macros is up to date (v{}).",
                        crate::update::current_version()
                    ),
                    cx,
                ),
                Ok(Some(available)) => editor.start_update_install(available, cx),
                Err(err) => editor.echo(format!("Update check failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Download, compile, and load a user tree-sitter grammar (M-x
    /// install-language → `(grammar-install …)`). The git clone + C compile run
    /// on the background executor so the UI stays live; only the result echo and
    /// the re-highlight touch the editor. On success the grammar is in the
    /// process-global registry, so any later file open of a matching extension
    /// highlights automatically (see `language_for_path`); here we additionally
    /// re-highlight the *current* buffer if it already visits such a file.
    /// Entry point for a grammar install. Installing runs arbitrary native code
    /// (the compiled parser + scanner are `dlopen`ed), so the first install from
    /// a given repo asks for confirmation in the minibuffer; approved sources are
    /// remembered (see [`crate::grammar::is_trusted`]) so a config-driven install
    /// on every launch doesn't re-prompt. Trusted → straight to the work.
    pub(super) fn install_grammar(
        &mut self,
        spec: crate::grammar::InstallSpec,
        cx: &mut Context<Self>,
    ) {
        if crate::grammar::is_trusted(&spec.repo) {
            self.run_grammar_install(spec, cx);
            return;
        }
        // Untrusted source: queue it and, if nothing is currently being prompted,
        // start the prompt. Queuing (rather than a single slot) means several
        // untrusted `install-language` calls in one `init.scm` are each prompted
        // in turn instead of all but the last being silently dropped.
        let was_idle = self.pending_grammar_installs.is_empty();
        self.pending_grammar_installs.push_back(spec);
        if was_idle {
            self.prompt_next_grammar_install(cx);
        }
    }

    /// Open the first-trust confirmation for the install at the front of the
    /// queue. The answer is handled by `MinibufferPurpose::ConfirmGrammarInstall`,
    /// which pops the front and chains to the next queued install.
    fn prompt_next_grammar_install(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.pending_grammar_installs.front() else {
            return;
        };
        let prompt = format!(
            "Install grammar {} from {} (downloads and runs native code)? (yes or no) ",
            spec.id, spec.repo
        );
        self.open_minibuffer(&prompt, MinibufferPurpose::ConfirmGrammarInstall, "", cx);
    }

    /// Resolve the front grammar-install confirmation with the user's answer,
    /// then prompt the next queued one. Called from the minibuffer submit path.
    pub(super) fn resolve_grammar_install_confirm(
        &mut self,
        approved: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(spec) = self.pending_grammar_installs.pop_front() {
            if approved {
                crate::grammar::trust(&spec.repo);
                self.run_grammar_install(spec, cx);
            } else {
                self.echo(format!("Skipped grammar {}", spec.id), cx);
            }
        }
        if !self.pending_grammar_installs.is_empty() {
            self.prompt_next_grammar_install(cx);
        }
    }

    /// Do the actual download + compile + load on the background executor, then
    /// re-highlight the current buffer and echo the result. Reached only after
    /// the source is trusted (directly, or via the confirm prompt).
    fn run_grammar_install(&mut self, spec: crate::grammar::InstallSpec, cx: &mut Context<Self>) {
        self.echo(
            format!("Installing grammar {} (clone + compile)…", spec.id),
            cx,
        );
        let task = cx
            .background_executor()
            .spawn(async move { crate::grammar::install(spec) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |editor, cx| match result {
                Ok(lang) => {
                    // Colorize the current buffer now if it visits a file the new
                    // grammar claims — otherwise it highlights on next open.
                    let matches = editor.active.path.as_deref().and_then(language_for_path)
                        == Some(lang.as_str());
                    if let Some(h) = matches.then(|| Highlighter::for_language(&lang)).flatten() {
                        editor.active.highlighter = Some(h);
                        editor.parse_full();
                    }
                    editor.echo(format!("Installed grammar {lang}."), cx);
                }
                Err(err) => editor.echo(format!("Grammar install failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Download + install an already-resolved [`crate::update::UpdateAvailable`],
    /// echoing progress. Driven by [`check_for_updates`].
    fn start_update_install(
        &mut self,
        available: crate::update::UpdateAvailable,
        cx: &mut Context<Self>,
    ) {
        self.echo(
            format!("Update v{} available — downloading…", available.version),
            cx,
        );
        let task = cx
            .background_executor()
            .spawn(async move { crate::update::install(&available, |_| {}) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |editor, cx| match outcome {
                Ok(crate::update::InstallOutcome::Staged) => {
                    let _ = crate::update::relaunch();
                    editor.echo("Update installed — restart Macros to finish.", cx);
                }
                Ok(crate::update::InstallOutcome::OpenedDownloadPage) => {
                    editor.echo("Opened the download page in your browser.", cx)
                }
                Err(err) => editor.echo(format!("Update failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }
}
