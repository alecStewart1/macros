mod analyze;
mod anvil;
mod assets;
mod buffer;
mod editor;
mod element;
mod grammar;
mod highlight;
mod lsp;
mod mode;
mod runtime;
mod script;
mod settings;
mod spell;
mod terminal;
mod theme;
mod update;
mod util;
mod workspace;

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, PromptLevel, WindowBounds, WindowHandle,
    WindowOptions, actions, prelude::*, px, size,
};

/// Bundled editor font (SIL OFL — see assets/fonts/Lilex-OFL.txt).
const LILEX_REGULAR: &[u8] = include_bytes!("../assets/fonts/Lilex-Regular.ttf");

use editor::{
    Backspace, Delete, Down, End, Home, Left, Newline, Right, SelectAll, SelectLeft, SelectRight,
    Tab, Up,
};
use workspace::Workspace;

// App-level actions, driven by the native menu bar / dock menu (and their
// `cmd-n` / `cmd-q` key equivalents). Distinct from the editor's `actions!`
// (OS-special keys) — these are handled globally, not per-editor.
actions!(macros, [NewWindow, Quit]);

/// The native application menu bar. macOS always renders the *first* menu as
/// the bold "application" menu (its title is replaced by the bundle name), so
/// its items are the app-level ones; the "File" menu carries `New Window`.
fn app_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Macros".into(),
            items: vec![MenuItem::action("Quit Macros", Quit)],
        },
        Menu {
            name: "File".into(),
            items: vec![MenuItem::action("New Window", NewWindow)],
        },
    ]
}

/// Open a fresh editor window and drive it through the same hidden-until-themed
/// reveal dance as the first one. Each window owns an independent Steel engine
/// (it lives on the `Editor`), so every new window must re-run the startup +
/// user config to get keymaps, theme, options, and any `(evil-mode)`. Only the
/// first window runs the launch-time update check (`run_update_check`).
fn open_workspace_window(cx: &mut App, run_update_check: bool) -> WindowHandle<Workspace> {
    let bounds = Bounds::centered(None, size(px(900.0), px(640.0)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // The window/title-bar name.
                // `appears_transparent` hides the system-drawn title bar so
                // we can paint our own strip in the theme's color (see
                // `Workspace::render`). The window stays a titled window, so
                // the top strip is still OS-draggable and the macOS traffic
                // lights still render in their default position.
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Macros".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                // Open hidden so the runtime + user config (which set the
                // theme via `load-theme`) run before the window is ever
                // presented. Otherwise the first frame paints with the dark
                // bootstrap palette and then visibly "blinks" to the user's
                // theme. We reveal the window with `activate_window()` once
                // theming is applied — the same trick Zed uses to avoid a
                // flash-of-unstyled-window.
                show: false,
                ..Default::default()
            },
            |_, cx| {
                let editor = cx.new(editor::Editor::new);
                cx.new(|cx| Workspace::new(editor, cx))
            },
        )
        .unwrap();

    // The window is opened hidden (`show: false`); it's revealed only once
    // the embedded runtime + the user's init.scm have applied their theme,
    // so the user never sees the dark bootstrap palette blink to their
    // configured theme.
    //
    // Reveal is a race between two tasks, guarded so it happens exactly once:
    //   (a) config finishes evaluating — the normal path, no flash; or
    //   (b) a watchdog timer elapses — a backstop so a slow/broken init.scm
    //       (or any error on the setup path) can never leave the window
    //       hidden forever; we show it even if that means a brief restyle.
    // Both run as foreground tasks so the run loop stays live between eval
    // stages. A single eval call that hangs internally (e.g. an infinite
    // loop in init.scm) still can't be preempted — the watchdog can only
    // fire at an `.await` point — which is the same way Emacs hangs on a bad
    // init.el.
    const WATCHDOG: Duration = Duration::from_millis(1500);
    let revealed = Arc::new(AtomicBool::new(false));

    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx);

            // The OS title-bar close button: intercept it so unsaved buffers
            // get the same save/cancel prompt as C-x C-c. With another window
            // still open, a clean close drops just this window (return true)
            // rather than quitting the whole app — `confirm_quit` calls
            // `cx.quit()`, which is only what we want for the last window. A
            // window with unsaved buffers still falls through to that prompt,
            // so nothing is discarded silently.
            let ws = cx.entity();
            window.on_window_should_close(cx, move |_window, cx| {
                let others_open = cx.windows().len() > 1;
                let dirty = ws.read(cx).has_unsaved_buffers(cx);
                if others_open && !dirty {
                    return true;
                }
                ws.update(cx, |workspace, cx| workspace.confirm_quit(cx))
            });

            // (b) Backstop: reveal after WATCHDOG regardless of config state.
            let guard = revealed.clone();
            cx.spawn_in(window, async move |_workspace, cx| {
                cx.background_executor().timer(WATCHDOG).await;
                if !guard.swap(true, Ordering::SeqCst) {
                    cx.update(|window, _| window.activate_window()).ok();
                }
            })
            .detach();

            // (a) Normal path: evaluate config, then reveal the already-
            // themed window. The yields between stages keep the run loop
            // alive so the watchdog above can still fire if a stage drags.
            let guard = revealed.clone();
            cx.spawn_in(window, async move |workspace, cx| {
                // Embedded runtime (keymaps, default theme, options).
                for source in runtime::startup_scripts() {
                    workspace.update(cx, |w, cx| w.eval(source, cx)).ok();
                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
                }
                // User config (overrides everything; may `(include …)` /
                // `(package-install …)`).
                if let Some((dir, source)) = runtime::user_config() {
                    let prelude = runtime::include_prelude(&dir);
                    workspace.update(cx, |w, cx| w.eval(&prelude, cx)).ok();
                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
                    workspace.update(cx, |w, cx| w.eval(&source, cx)).ok();
                }

                // Emacs `after-init-hook`: session restore (desktop + savehist)
                // runs here — after the user's init.scm could opt out, and
                // synchronously, so the restored windows are in place before
                // keys are let through and before the reveal.
                workspace.update(cx, |w, cx| w.run_after_init(cx)).ok();

                // Config (incl. any `(evil-mode)`) is fully applied: let
                // plain keystrokes through again. Done before the reveal so
                // the very first key the user can press is already modal.
                workspace.update(cx, |w, cx| w.mark_config_loaded(cx)).ok();

                // Surface the dyld-hijack tripwire in the echo area too —
                // stderr is invisible for Finder/dock launches, and this
                // failure mode otherwise presents as a random paint crash.
                #[cfg(target_os = "macos")]
                if let Some(warning) = dyld_hijack_warning() {
                    let scheme = format!(
                        "(message \"{}\")",
                        warning.replace('\\', " ").replace('"', "'")
                    );
                    workspace.update(cx, |w, cx| w.eval(&scheme, cx)).ok();
                }

                // Theme is applied — reveal the styled window (unless the
                // watchdog already did).
                if !guard.swap(true, Ordering::SeqCst) {
                    cx.update(|window, _| window.activate_window()).ok();
                }

                // Only now run the initial buffer's mode hooks (steel-mode's
                // `enable-lsp` → Steel engine / LSP). Deferred past the reveal
                // so the first frame paints before that work begins.
                cx.background_executor()
                    .timer(Duration::from_millis(1))
                    .await;
                workspace.update(cx, |w, cx| w.run_startup_hooks(cx)).ok();
            })
            .detach();

            // Update lifecycle (relocation prompt / once-a-day check):
            // see `spawn_update_check`. First window only.
            if run_update_check {
                spawn_update_check(window, cx);
            }
        })
        .unwrap();

    window
}

/// The DYLD_LIBRARY_PATH hijack warning text, when the environment carries
/// third-party library dirs that shadow system frameworks' dependents — the
/// known crash vector is Homebrew's libpng vs ImageIO during emoji
/// rasterization. `None` when the environment is clean.
#[cfg(target_os = "macos")]
fn dyld_hijack_warning() -> Option<String> {
    let v = std::env::var("DYLD_LIBRARY_PATH").ok()?;
    if v.split(':')
        .any(|p| p.contains("/homebrew") || p == "/usr/local/lib")
    {
        Some(format!(
            "DYLD_LIBRARY_PATH includes third-party library dirs ({v}) — these \
             shadow Apple's ImageIO codecs and crash emoji rendering (SIGBUS in \
             PNGReadPlugin). Remove the export from your shell config, or use \
             DYLD_FALLBACK_LIBRARY_PATH instead."
        ))
    } else {
        None
    }
}

fn main() {
    // Internal spell-helper mode, run as a child process by src/spell.rs:
    // `--spell-suggest <word>` prints correction candidates one per line;
    // `--spell-learn <word>` adds a word to the OS user dictionary. The
    // synchronous NSSpellChecker calls behind these work in a plain process
    // but wedge permanently inside the running app (see src/spell.rs), so
    // the app shells out to itself. Exits before any UI setup.
    {
        let mut args = std::env::args().skip(1);
        match (args.next(), args.next()) {
            (Some(flag), Some(word)) if flag == "--spell-suggest" => {
                for s in spell::guesses(&word) {
                    println!("{s}");
                }
                return;
            }
            (Some(flag), Some(word)) if flag == "--spell-learn" => {
                spell::learn(&word);
                return;
            }
            _ => {}
        }
    }

    // Recover the login shell's PATH before anything else — GUI launches on
    // macOS/Linux don't inherit it, which otherwise hides LSP servers and other
    // tools from every subprocess we spawn. Must run before any threads start.
    runtime::import_shell_env();

    // dyld hijack tripwire (macOS): a globally exported DYLD_LIBRARY_PATH
    // pointing at Homebrew substitutes Homebrew's libpng/libtiff/… for the
    // copies Apple's ImageIO expects (dyld overrides by LEAF NAME), and the
    // first color-emoji glyph the editor rasterizes then dies with SIGBUS at
    // a poisoned pointer inside PNGReadPlugin — a crash that looks like ours
    // but is entirely environmental. dyld consumed the variable before main,
    // so it can't be undone here; it CAN be named loudly instead of the app
    // dying mysteriously mid-paint. (Fix: remove the export from the shell
    // config; DYLD_FALLBACK_LIBRARY_PATH is the safe variant if a tool needs
    // Homebrew's libs.)
    #[cfg(target_os = "macos")]
    if let Some(warning) = dyld_hijack_warning() {
        eprintln!("macros: {warning}");
    }

    let app = Application::new().with_assets(assets::Assets);

    // Clicking the dock icon (macOS) with every window closed opens a fresh
    // one; otherwise just bring the app forward. Registered on the builder —
    // `on_reopen` lives on `Application`, not the in-run `App`.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            open_workspace_window(cx, false);
        }
        cx.activate(true);
    });

    app.run(|cx: &mut App| {
        // gpui ships no usable default font for the text system — fonts must be
        // loaded explicitly or glyphs won't rasterize. Bundle Lilex (a mono,
        // ideal for a code editor).
        cx.text_system()
            .add_fonts(vec![Cow::Borrowed(LILEX_REGULAR)])
            .expect("Lilex font should load");

        // The color theme lives as a gpui global, but its palette is defined in
        // Scheme (scheme/themes/*.scm, applied by `(load-theme …)` in init.scm).
        // Rust installs an empty palette as a bootstrap; faces fall back to
        // sane defaults for the first frame until the runtime loads.
        cx.set_global(theme::Theme::default());

        // Tunable options (font size, tab width, completion style, …) are also a
        // gpui global; defaults live in Rust, overrides come from Scheme
        // `(set-option …)` in init.scm.
        cx.set_global(settings::Settings::default());

        // OS-special keys are captured as GPUI actions and delegated to the
        // same commands. The `Ctrl-`/`Alt-`/prefix keymap lives in Scheme
        // (FUNDAMENTAL_KEYMAP) and is dispatched inside the editor.
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some("Editor")),
            KeyBinding::new("delete", Delete, Some("Editor")),
            KeyBinding::new("left", Left, Some("Editor")),
            KeyBinding::new("right", Right, Some("Editor")),
            KeyBinding::new("up", Up, Some("Editor")),
            KeyBinding::new("down", Down, Some("Editor")),
            KeyBinding::new("home", Home, Some("Editor")),
            KeyBinding::new("end", End, Some("Editor")),
            KeyBinding::new("enter", Newline, Some("Editor")),
            KeyBinding::new("tab", Tab, Some("Editor")),
            KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
            KeyBinding::new("shift-right", SelectRight, Some("Editor")),
            KeyBinding::new("cmd-a", SelectAll, Some("Editor")),
            // App-level shortcuts (no context → dispatched regardless of
            // focus). These also become the key equivalents the menu bar
            // displays next to `New Window` / `Quit Macros`.
            KeyBinding::new("cmd-n", NewWindow, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        // Native menu bar + dock (macOS; no-ops elsewhere). Both `New Window`
        // entries dispatch the same `NewWindow` action.
        cx.set_menus(app_menus());
        cx.set_dock_menu(vec![MenuItem::action("New Window", NewWindow)]);

        cx.on_action::<NewWindow>(|_, cx| {
            open_workspace_window(cx, false);
        });
        cx.on_action::<Quit>(|_, cx| {
            // Route through the focused window so unsaved buffers still
            // prompt, matching the title-bar close button and C-x C-c.
            // No active window → plain quit.
            if let Some(window) = cx.active_window().and_then(|w| w.downcast::<Workspace>()) {
                window
                    .update(cx, |workspace, _window, cx| workspace.confirm_quit(cx))
                    .ok();
            } else {
                cx.quit();
            }
        });

        open_workspace_window(cx, true);

        cx.activate(true);
    });
}

/// Update lifecycle, spawned once at startup: on macOS, offer to relocate a
/// bundle stuck on a read-only volume (a mounted DMG or a Gatekeeper-
/// translocated download can't self-update); otherwise run a once-a-day check
/// against macros.dev and surface a newer release as a title-bar pill. All
/// network/IO happens on the background executor; the modal prompts run on the
/// main thread between awaits. Detached so it never blocks the startup reveal
/// path.
fn spawn_update_check(window: &mut gpui::Window, cx: &mut gpui::Context<Workspace>) {
    cx.spawn_in(window, async move |workspace, cx| {
        // Let the window reveal before any modal appears.
        cx.background_executor().timer(Duration::from_secs(2)).await;

        // macOS: a bundle on a read-only volume (mounted DMG or
        // Gatekeeper App Translocation) can't self-update. Offer to
        // move it into /Applications and relaunch from there; if the
        // user accepts, we quit and skip the update check this run.
        if update::needs_relocation() {
            let answer = cx
                .update(|window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        "Move Macros to the Applications folder?",
                        Some(
                            "Macros is running from a read-only location (a disk \
                         image or a quarantined download), so automatic updates \
                         can't be installed. Macros can move itself to \
                         Applications and relaunch from there.",
                        ),
                        &["Move to Applications", "Not Now"],
                        cx,
                    )
                })
                .ok();
            if let Some(rx) = answer
                && matches!(rx.await, Ok(0))
            {
                let moved = cx
                    .background_executor()
                    .spawn(async { update::relocate() })
                    .await;
                match moved {
                    Ok(()) => {
                        cx.update(|_, cx| cx.quit()).ok();
                    }
                    Err(err) => {
                        cx.update(|window, cx| {
                            window.prompt(
                                PromptLevel::Critical,
                                "Couldn't move Macros to Applications",
                                Some(&format!(
                                    "{err}\n\nYou can move Macros.app there yourself \
                                 with Finder."
                                )),
                                &["OK"],
                                cx,
                            )
                        })
                        .ok();
                    }
                }
            }
            return;
        }

        // Once-a-day update check, honoring the `auto-update`
        // option (default on; `(set-auto-update #f)` turns it
        // off). Silent on network error so a flaky connection
        // never nags the user at launch.
        let auto_update = cx
            .update(|_, cx| cx.global::<settings::Settings>().auto_update())
            .unwrap_or(true);
        if !auto_update {
            return;
        }
        // Skip silently when the once-a-day gate hasn't elapsed —
        // only announce a check in `*Messages*` when we actually
        // hit the network this launch.
        if !update::due_for_check() {
            return;
        }
        workspace
            .update(cx, |ws, cx| ws.log_message("Checking for updates…", cx))
            .ok();
        let found = cx
            .background_executor()
            .spawn(async { update::check_gated() })
            .await;
        let available = match found {
            Ok(Some(available)) => available,
            // Already current: report it, then stop. Errors stay
            // quiet so a flaky connection never nags at launch.
            Ok(None) => {
                workspace
                    .update(cx, |ws, cx| {
                        ws.log_message(
                            format!("Macros is up to date (v{}).", update::current_version()),
                            cx,
                        )
                    })
                    .ok();
                return;
            }
            Err(_) => return,
        };

        // Don't download anything yet — just surface the update
        // as a pill on the right of the title bar. The user clicks
        // it to download (with a live progress ticker) and then to
        // restart; that flow lives in `Workspace`.
        workspace
            .update(cx, |ws, cx| {
                ws.log_message(format!("Macros {} is available.", available.version), cx);
                ws.show_update_available(available, cx);
            })
            .ok();
    })
    .detach();
}
