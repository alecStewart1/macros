use gpui::{Context, Entity, SharedString, Window, div, prelude::*, px, relative};

use crate::editor::Editor;
use crate::theme::Theme;

/// The echo-area row beneath the minibuffer/mode line: a full-width line that
/// wraps long messages (LSP diagnostics, completion hints) instead of clipping
/// them at the window edge, capped at a few lines so it can't swallow the
/// editor. The full history remains in `*Messages*`.
fn echo_row(text: SharedString, color: gpui::Hsla, bg: gpui::Hsla) -> impl IntoElement {
    div()
        .w_full()
        .px_2()
        .py(px(2.))
        .max_h(px(96.))
        .overflow_hidden()
        .bg(bg)
        .text_color(color)
        .text_size(px(13.))
        .child(text)
}

/// The theme colors the bottom-region renderers share. Resolved once per frame
/// in `render` and threaded into each helper so every region uses identical
/// colors (and we read the `Theme` global only once).
#[derive(Clone, Copy)]
struct Colors {
    background: gpui::Hsla,
    mode_line_bg: gpui::Hsla,
    mode_line_fg: gpui::Hsla,
    prompt_color: gpui::Hsla,
    cursor_color: gpui::Hsla,
    dim: gpui::Hsla,
}

/// What clicking the update notice does, decided by the launch-time check when
/// it finds a newer release on macros.dev.
#[derive(Clone)]
pub enum UpdateAction {
    /// macOS: the newer build is already staged in place — relaunch to apply it.
    /// Only constructed on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Restart,
    /// Windows/Linux: no in-place swap, so open the downloads page in a browser.
    /// Never constructed on macOS (which stages in place).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    OpenUrl(String),
}

/// A dismissible "update available" box painted in the top-right corner.
#[derive(Clone)]
struct UpdateNotice {
    version: SharedString,
    action: UpdateAction,
}

/// The top-level frame: an editor filling the window above a status line. This
/// is the root view GPUI renders, analogous to an Emacs frame containing one
/// window and the mode line.
pub struct Workspace {
    editor: Entity<Editor>,
    /// Set by the launch-time update check; rendered as a top-right notice until
    /// the user clicks it (restart / download) or dismisses it with the `✕`.
    update_notice: Option<UpdateNotice>,
}

impl Workspace {
    pub fn new(editor: Entity<Editor>, cx: &mut Context<Self>) -> Self {
        // Re-render the frame (status/mode line, minibuffer) whenever the editor
        // changes, so it reflects status messages, mode state, and theme edits.
        cx.observe(&editor, |_, _, cx| cx.notify()).detach();
        Self {
            editor,
            update_notice: None,
        }
    }

    /// Log a status message to the editor's echo area and `*Messages*` buffer.
    /// Used by the launch-time update flow so its progress (checking, downloading)
    /// is visible in `*Messages*` like the interactive `check-for-updates` command.
    pub fn log_message(&self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.message(message, cx));
    }

    /// Show the dismissible top-right "update available" notice. Called by the
    /// launch-time update check once a newer release is staged (macOS) or
    /// resolved (other platforms).
    pub fn show_update_notice(
        &mut self,
        version: impl Into<SharedString>,
        action: UpdateAction,
        cx: &mut Context<Self>,
    ) {
        self.update_notice = Some(UpdateNotice {
            version: version.into(),
            action,
        });
        cx.notify();
    }

    /// Act on the update notice: relaunch into the staged build (macOS) or open
    /// the downloads page (other platforms). Dismisses the notice either way.
    fn activate_update_notice(&mut self, action: &UpdateAction, cx: &mut Context<Self>) {
        self.update_notice = None;
        cx.notify();
        match action {
            UpdateAction::Restart => {
                let _ = crate::update::relaunch();
                cx.quit();
            }
            UpdateAction::OpenUrl(url) => {
                let _ = crate::update::open_url(url);
            }
        }
    }

    /// Move keyboard focus to the editor. Called once after the window opens.
    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.editor.read(cx).focus_handle.clone();
        window.focus(&handle);
    }

    /// Run a Steel snippet against the editor synchronously (startup config:
    /// keymaps, theme, options must apply in order before the UI is driven).
    pub fn eval(&self, source: &str, cx: &mut Context<Self>) {
        let source = source.to_string();
        self.editor.update(cx, |editor, cx| {
            editor.run_script_blocking(&source, cx);
        });
    }

    /// Run the initial buffer's mode hooks, after the startup + user scripts have
    /// registered them (this is what brings up the Steel LSP for `*scratch*`).
    pub fn run_startup_hooks(&self, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.run_mode_hooks_once(cx));
    }

    /// An OS window-close request: defer to the editor, which cancels the close
    /// and prompts to save when buffers are modified (see `Editor::confirm_quit`).
    pub fn confirm_quit(&self, cx: &mut Context<Self>) -> bool {
        self.editor.update(cx, |editor, cx| editor.confirm_quit(cx))
    }

    /// The transient popup (magit-style command menu): a titled grid of
    /// key/label rows laid out as columns. Each row is clickable and dispatches
    /// the bound command via `Editor::transient_click`.
    fn render_transient(
        &self,
        transient: crate::editor::TransientView,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let panel_bg = theme.color("completion-bg", colors.background);
        let key_color = theme.color("magit-key", colors.prompt_color);
        let foreground = theme.foreground();
        let dim = colors.dim;
        // Build each column as a flex-col; lay the columns out in a row.
        let mut columns_row = div().flex().flex_row().gap_4().px_2();
        for column in transient.columns {
            let mut col = div().flex().flex_col();
            if let Some(title) = column.title {
                col = col.child(div().text_color(dim).child(SharedString::from(title)));
            }
            for row in column.rows {
                let command = row.command.clone();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .cursor(gpui::CursorStyle::PointingHand)
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev, _window, cx| {
                                let command = command.clone();
                                this.editor
                                    .update(cx, |editor, cx| editor.transient_click(&command, cx));
                                cx.stop_propagation();
                            }),
                        )
                        .child(
                            div()
                                .text_color(key_color)
                                .min_w(px(48.))
                                .child(SharedString::from(row.key)),
                        )
                        .child(SharedString::from(row.label)),
                );
            }
            columns_row = columns_row.child(col);
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .bg(panel_bg)
            .text_color(foreground)
            .text_size(px(13.))
            .px_2()
            .py_1()
            .child(div().text_color(dim).child(SharedString::from(format!(
                "{} — transient",
                transient.title
            ))))
            .child(columns_row)
            .into_any_element()
    }

    /// The Helm selection panel: a beveled, fixed-height box with a prompt +
    /// live filter pattern on top, candidate rows beneath, and an optional
    /// preview pane on the right. Rows are clickable and run the default action.
    fn render_helm(
        &self,
        helm: crate::editor::HelmView,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let background = colors.background;
        let foreground = theme.foreground();
        let prompt_color = colors.prompt_color;
        let cursor_color = colors.cursor_color;
        let dim = colors.dim;
        let settings = cx.global::<crate::settings::Settings>();
        let helm_height = settings.helm_height();
        let helm_bevel_top = settings.helm_bevel_top();
        let helm_bevel_left = settings.helm_bevel_left();
        let helm_bevel_right = settings.helm_bevel_right();
        let helm_bevel_bottom = settings.helm_bevel_bottom();
        let panel_bg = theme.color("completion-bg", background);
        let selected_bg = theme.color("completion-selected-bg", theme.color("region", background));
        let selected_fg = theme.color("completion-selected-fg", theme.foreground());
        // Bevel: a light top/left edge and a dark bottom/right edge give the
        // panel a raised look. Set both faces to the panel bg for a flat one.
        let bevel_light = theme.color("helm-bevel-light", panel_bg);
        let bevel_shadow = theme.color("helm-bevel-shadow", panel_bg);
        // Optional preview pane (right half) for the current selection.
        let preview_pane = helm.preview.map(|text| {
            div()
                .flex_1()
                .px_2()
                .py_1()
                .text_color(dim)
                .bg(panel_bg)
                .child(SharedString::from(text))
        });

        let mut content = div()
            .flex_1()
            .flex()
            .flex_col()
            .bg(panel_bg)
            .text_color(foreground)
            .text_size(px(13.))
            // Prompt + live filter pattern.
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .text_color(prompt_color)
                            .child(SharedString::from(helm.prompt)),
                    )
                    .child(SharedString::from(helm.pattern))
                    .child(div().w(px(2.)).h(px(15.)).bg(cursor_color)),
            );
        for row in helm.rows {
            // A leading "* " marks multi-selected candidates (C-SPC).
            let prefix = if row.marked { "* " } else { "  " };
            let text: SharedString = format!("{prefix}{}", row.text).into();
            // A marginalia-style annotation, pushed to the right and dimmed.
            let annotation = row.annotation.clone();
            let with_annotation = |line: gpui::Div| match &annotation {
                Some(ann) if !ann.is_empty() => line
                    .flex()
                    .flex_row()
                    .justify_between()
                    .child(text.clone())
                    .child(
                        div()
                            .pl_4()
                            .text_color(dim)
                            .child(SharedString::from(ann.clone())),
                    ),
                _ => line.child(text.clone()),
            };
            let line = if row.header {
                div()
                    .px_2()
                    .text_color(dim)
                    .child(SharedString::from(format!("— {} —", row.text)))
            } else if row.selected {
                with_annotation(
                    div()
                        .px_4()
                        .w_full()
                        .bg(selected_bg)
                        .text_color(selected_fg),
                )
            } else if row.marked {
                with_annotation(div().px_4().w_full().text_color(prompt_color))
            } else {
                with_annotation(div().px_4().w_full())
            };
            // Candidate rows are clickable: select and run the default action.
            let line = match row.index {
                Some(index) => line.cursor(gpui::CursorStyle::PointingHand).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _ev, _window, cx| {
                        this.editor
                            .update(cx, |editor, cx| editor.helm_click(index, cx));
                        cx.stop_propagation();
                    }),
                ),
                None => line,
            };
            content = content.child(line);
        }
        // Beveled frame: light top/left edges, dark bottom/right edges, each a
        // configurable-width colored strip. The panel takes `helm-height` of
        // the window; the editor (flex) fills the rest.
        div()
            .w_full()
            .h(relative(helm_height))
            .flex()
            .flex_col()
            // Fill the whole box so empty space below the rows is panel bg.
            .bg(panel_bg)
            .child(div().w_full().h(px(helm_bevel_top)).bg(bevel_light))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .child(div().w(px(helm_bevel_left)).bg(bevel_light))
                    .child(content)
                    .children(preview_pane)
                    .child(div().w(px(helm_bevel_right)).bg(bevel_shadow))
                    // The content row fills the panel; its bg shows under rows.
                    .bg(panel_bg),
            )
            .child(div().w_full().h(px(helm_bevel_bottom)).bg(bevel_shadow))
            .into_any_element()
    }

    /// The minibuffer: a fixed-height input row (prompt + split input + caret),
    /// with any trailing message wrapped onto its own echo row below so long
    /// text (e.g. completion hints) is readable instead of clipped.
    fn render_minibuffer(
        &self,
        mb: crate::editor::MinibufferView,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let background = colors.background;
        let foreground = theme.foreground();
        let prompt_color = colors.prompt_color;
        let cursor_color = colors.cursor_color;
        let dim = colors.dim;
        // The input line (prompt + split input + caret) stays one fixed row;
        // any trailing message wraps onto its own echo row below it so long
        // text (e.g. completion hints) is readable instead of clipped.
        let input = div()
            .h(px(24.))
            .w_full()
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .child(div().text_color(prompt_color).child(mb.prompt))
            .child(mb.before)
            // A thin caret between the two halves of the input.
            .child(div().w(px(2.)).h(px(15.)).bg(cursor_color))
            .child(mb.after);
        let mut col = div()
            .w_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .text_size(px(13.))
            .child(input);
        if !mb.message.is_empty() {
            col = col.child(echo_row(mb.message.into(), dim, background));
        }
        col.into_any_element()
    }

    /// The default bottom region: a fixed-height mode line carrying the
    /// buffer/mode/diagnostic summary, with the echo message (e.g. the LSP
    /// diagnostic at point) on its own wrapping row below it.
    fn render_status_line(&self, colors: Colors, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let foreground = theme.foreground();
        let editor = self.editor.read(cx);
        // Emacs-style modified marker: `**` modified, `--` saved.
        let marker = if editor.is_modified() { "**" } else { "--" };
        let diagnostics = editor
            .diagnostic_summary()
            .map(|s| format!("  {s}"))
            .unwrap_or_default();
        let evil = editor
            .evil_label()
            .map(|l| format!("{l}  "))
            .unwrap_or_default();
        // Projectile-style project indicator, e.g. "[macros]".
        let project = editor
            .project_indicator()
            .map(|p| format!("  [{p}]"))
            .unwrap_or_default();
        let modes: SharedString = format!(
            "{evil}{marker}  {}  ({}){project}{diagnostics}",
            editor.buffer_name(),
            editor.modes.names().join(" ")
        )
        .into();
        let message = editor.status.clone();
        // Mode line: fixed-height strip carrying buffer/mode/diagnostic
        // summary. The echo message (e.g. the LSP diagnostic at point) lives
        // on its own wrapping row below, Emacs-style, so it no longer fights
        // the mode line for space or bleeds off the window's right edge.
        let mode_line = div()
            .h(px(24.))
            .w_full()
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .bg(colors.mode_line_bg)
            .text_color(colors.mode_line_fg)
            .text_size(px(13.))
            .child(modes);
        let mut col = div().w_full().flex().flex_col().child(mode_line);
        if !message.is_empty() {
            col = col.child(echo_row(message, foreground, colors.background));
        }
        col.into_any_element()
    }

    /// The dismissible "update available" box anchored to the top-right of the
    /// editor area. The message acts as the affirmative button (restart /
    /// download); the trailing `✕` just hides the notice until next launch.
    fn render_update_notice(
        &self,
        notice: UpdateNotice,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.global::<Theme>();
        let foreground = theme.foreground();
        let panel_bg = theme.color("completion-bg", colors.mode_line_bg);
        let border = theme.color("dim", colors.dim);
        let label: SharedString = match &notice.action {
            UpdateAction::Restart => {
                format!("Macros {} ready — click to restart", notice.version).into()
            }
            UpdateAction::OpenUrl(_) => {
                format!("Macros {} available — click to download", notice.version).into()
            }
        };
        let action = notice.action;
        div()
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(panel_bg)
            .border_1()
            .border_color(border)
            .rounded_md()
            .text_size(px(13.))
            .text_color(foreground)
            .child(
                div()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _ev, _window, cx| {
                            this.activate_update_notice(&action, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(label),
            )
            .child(
                div()
                    .px_1()
                    .cursor(gpui::CursorStyle::PointingHand)
                    .text_color(colors.dim)
                    .child(SharedString::from("✕"))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _ev, _window, cx| {
                            this.update_notice = None;
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_family = cx
            .global::<crate::settings::Settings>()
            .font_family()
            .to_string();
        let theme = cx.global::<Theme>();
        let background = theme.background();
        let colors = Colors {
            background,
            mode_line_bg: theme.color("mode-line-bg", theme.background()),
            mode_line_fg: theme.color("mode-line-fg", theme.foreground()),
            prompt_color: theme.color("minibuffer-prompt", theme.foreground()),
            cursor_color: theme.color("cursor", theme.foreground()),
            dim: theme.color("dim", theme.foreground()),
        };

        // Pull the active bottom-region view (owned) before building it, so no
        // `&Editor` borrow is held across the `cx.listener` calls in the helpers.
        let transient = self.editor.read(cx).transient_view();
        let helm = self.editor.read(cx).helm_view();
        let minibuffer = self.editor.read(cx).minibuffer_view();

        // The bottom region is, in priority order: a transient popup, the Helm
        // selection panel, the minibuffer, or the status / mode line.
        let bottom = if let Some(transient) = transient {
            self.render_transient(transient, colors, cx)
        } else if let Some(helm) = helm {
            self.render_helm(helm, colors, cx)
        } else if let Some(mb) = minibuffer {
            self.render_minibuffer(mb, colors, cx)
        } else {
            self.render_status_line(colors, cx)
        };

        // Custom title-bar strip. The system title bar is transparent (see
        // `main.rs`), so we paint this ourselves in the theme's `title-bar-bg`
        // and leave room on the left for the macOS traffic lights. The strip
        // stays draggable like a native title bar.
        let theme = cx.global::<Theme>();
        let editor = self.editor.read(cx);
        let title: SharedString = format!("Macros — {}", editor.buffer_name()).into();
        let title_bar = div()
            .w_full()
            .h(px(28.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            // Leave space for the macOS traffic lights at the top-left.
            .pl(px(72.))
            .pr(px(72.))
            .bg(theme.title_bar_bg())
            .text_color(theme.title_bar_fg())
            .text_size(px(13.))
            .window_control_area(gpui::WindowControlArea::Drag)
            .child(title);

        // The editor fills the central region; the update notice (if any) is
        // overlaid in its top-right corner via absolute positioning, so it
        // floats above the buffer without taking layout space.
        let mut editor_area = div().flex_1().relative().child(self.editor.clone());
        if let Some(notice) = self.update_notice.clone() {
            editor_area = editor_area.child(self.render_update_notice(notice, colors, cx));
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(background)
            .font_family(font_family)
            .child(title_bar)
            .child(editor_area)
            .child(bottom)
    }
}
