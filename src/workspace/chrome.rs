//! Window chrome the app draws itself: the Windows/Linux caption buttons and
//! the Linux client-side resize frame.

use super::*;

/// Width of the Windows caption buttons (three 46px buttons, the system size)
/// that `caption_buttons` draws at the title bar's right edge; zero elsewhere.
pub(super) const CAPTION_BUTTONS_WIDTH: gpui::Pixels = px(if cfg!(target_os = "windows") {
    138.
} else {
    0.
});

/// Minimize / maximize-restore / close for Windows. The transparent title bar
/// (see `main.rs`) drops the system-drawn caption buttons there, so we paint
/// them; each tags a `WindowControlArea`, and gpui answers `WM_NCHITTEST`
/// with the matching `HT*BUTTON`, so the OS handles the click itself.
pub(super) fn caption_buttons(window: &Window, fg: gpui::Hsla) -> impl IntoElement {
    // Segoe Fluent Icons (Windows 11) / MDL2 Assets (Windows 10) glyphs.
    let max_glyph = if window.is_maximized() {
        "\u{e923}"
    } else {
        "\u{e922}"
    };
    let button = |glyph: &'static str,
                  area: gpui::WindowControlArea,
                  hover_bg: gpui::Hsla,
                  hover_fg: gpui::Hsla| {
        div()
            .id(glyph)
            .w(px(46.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family("Segoe Fluent Icons")
            .text_size(px(10.))
            .window_control_area(area)
            .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
            .child(glyph)
    };
    let hover = fg.opacity(0.12);
    let close_red: gpui::Hsla = gpui::rgb(0xc42b1c).into();
    div()
        .occlude()
        .absolute()
        .right_0()
        .top_0()
        .h_full()
        .flex()
        .flex_row()
        .child(button("\u{e921}", gpui::WindowControlArea::Min, hover, fg))
        .child(button(max_glyph, gpui::WindowControlArea::Max, hover, fg))
        .child(button(
            "\u{e8bb}",
            gpui::WindowControlArea::Close,
            close_red,
            gpui::white(),
        ))
}

/// Room on the title bar's left for the macOS traffic lights; a plain margin
/// on platforms whose window buttons sit at the right.
pub(super) const TITLE_LEFT_INSET: gpui::Pixels =
    px(if cfg!(target_os = "macos") { 72. } else { 8. });

/// Width of each Linux caption button that `linux_caption_buttons` draws.
const LINUX_CAPTION_BUTTON_WIDTH: gpui::Pixels = px(36.);

/// Thickness of the invisible border around a client-decorated (Linux)
/// window that grabs the mouse for resizing. Also the room the drop shadow
/// paints into, so it's dropped on edges the compositor has tiled.
const RESIZE_BORDER: gpui::Pixels = px(8.);

/// Minimize / maximize-restore / close for Linux when we draw the window
/// decorations ourselves (see `main.rs`). Linux has no hit-test hook like
/// Windows' `WM_NCHITTEST`, so each button acts on a click. Wayland reports
/// which controls the compositor supports; close is always offered.
pub(super) fn linux_caption_buttons(
    window: &Window,
    fg: gpui::Hsla,
    cx: &mut Context<Workspace>,
) -> (gpui::Pixels, gpui::AnyElement) {
    let controls = window.window_controls();
    let max_icon = if window.is_maximized() {
        "icons/copy.svg"
    } else {
        "icons/square.svg"
    };
    let button =
        |id: &'static str, icon: &'static str, hover_bg: gpui::Hsla, hover_fg: gpui::Hsla| {
            div()
                .id(id)
                .w(LINUX_CAPTION_BUTTON_WIDTH)
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .group(id)
                .hover(move |style| style.bg(hover_bg))
                .child(
                    gpui::svg()
                        .path(icon)
                        .size(px(14.))
                        .text_color(fg)
                        .group_hover(id, move |style| style.text_color(hover_fg)),
                )
        };
    let hover = fg.opacity(0.12);
    let mut row = div()
        .occlude()
        .absolute()
        .right_0()
        .top_0()
        .h_full()
        .flex()
        .flex_row();
    let mut count = 1.;
    if controls.minimize {
        count += 1.;
        row = row.child(
            button("caption-minimize", "icons/minus.svg", hover, fg)
                .on_click(|_, window, _| window.minimize_window()),
        );
    }
    if controls.maximize {
        count += 1.;
        row = row.child(
            button("caption-maximize", max_icon, hover, fg)
                .on_click(|_, window, _| window.zoom_window()),
        );
    }
    row = row.child(
        button(
            "caption-close",
            "icons/x.svg",
            gpui::rgb(0xc42b1c).into(),
            gpui::white(),
        )
        .on_click(cx.listener(|this, _, window, cx| {
            if this.should_close_window(cx) {
                window.remove_window();
            }
        })),
    );
    (LINUX_CAPTION_BUTTON_WIDTH * count, row.into_any_element())
}

/// Which resize edge (if any) of a client-decorated window `pos` falls on.
/// Tiled edges sit flush against the screen or a neighbor, carry no border,
/// and never resize.
fn resize_edge(
    pos: gpui::Point<gpui::Pixels>,
    size: gpui::Size<gpui::Pixels>,
    tiling: gpui::Tiling,
) -> Option<gpui::ResizeEdge> {
    use gpui::ResizeEdge;
    let top = !tiling.top && pos.y < RESIZE_BORDER;
    let bottom = !tiling.bottom && pos.y > size.height - RESIZE_BORDER;
    let left = !tiling.left && pos.x < RESIZE_BORDER;
    let right = !tiling.right && pos.x > size.width - RESIZE_BORDER;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (.., true, _) => ResizeEdge::Left,
        (.., true) => ResizeEdge::Right,
        _ => return None,
    })
}

/// Wrap the workspace in the invisible resize border and drop shadow a
/// client-decorated (Linux) window draws for itself. The border is
/// transparent window surface; `set_client_inset` tells the compositor it
/// isn't part of the window proper (for snapping and the input region).
pub(super) fn client_frame(
    content: gpui::Div,
    tiling: gpui::Tiling,
    edge_color: gpui::Hsla,
    window: &mut Window,
) -> gpui::AnyElement {
    window.set_client_inset(RESIZE_BORDER);
    // Paint the resize cursor while the mouse is over the border.
    let cursor = gpui::canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let Some(edge) = resize_edge(window.mouse_position(), window.viewport_size(), tiling)
            else {
                return;
            };
            use gpui::{CursorStyle, ResizeEdge};
            let style = match edge {
                ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
                ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
            };
            window.set_cursor_style(style, &hitbox);
        },
    )
    .absolute()
    .size_full();

    let content = content
        .border_color(edge_color)
        .when(!tiling.top, |d| d.border_t_1())
        .when(!tiling.bottom, |d| d.border_b_1())
        .when(!tiling.left, |d| d.border_l_1())
        .when(!tiling.right, |d| d.border_r_1())
        .when(!tiling.is_tiled(), |d| {
            d.shadow(vec![gpui::BoxShadow {
                color: gpui::black().opacity(0.4),
                blur_radius: RESIZE_BORDER / 2.,
                spread_radius: px(0.),
                offset: gpui::point(px(0.), px(0.)),
            }])
        })
        // Moves over the content needn't repaint the border cursor below.
        .on_mouse_move(|_, _, cx| cx.stop_propagation());

    div()
        .id("window-frame")
        .size_full()
        .when(!tiling.top, |d| d.pt(RESIZE_BORDER))
        .when(!tiling.bottom, |d| d.pb(RESIZE_BORDER))
        .when(!tiling.left, |d| d.pl(RESIZE_BORDER))
        .when(!tiling.right, |d| d.pr(RESIZE_BORDER))
        .child(cursor)
        // Re-run the cursor canvas as the mouse crosses the border.
        .on_mouse_move(|_, window, _| window.refresh())
        .on_mouse_down(gpui::MouseButton::Left, move |e, window, _| {
            if let Some(edge) = resize_edge(e.position, window.viewport_size(), tiling) {
                window.start_window_resize(edge);
            }
        })
        .child(content)
        .into_any_element()
}
