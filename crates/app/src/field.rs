//! The text field used everywhere: gpui-ce's single-line `text_input`, styled as the box
//! itself so a click anywhere in it focuses it, long text scrolls to keep the caret in
//! view, and the border shows which field has focus.

use gpui::{App, ElementId, Entity, Focusable, SharedString, Window, prelude::*};
use gpui_elements::editable_text::{EditableTextElement, EditableTextState, text_input};

use crate::theme::{Theme, with_alpha};

pub fn field(
    id: impl Into<ElementId>,
    state: &Entity<EditableTextState>,
    placeholder: impl Into<SharedString>,
    theme: &Theme,
    window: &Window,
    cx: &App,
) -> EditableTextElement {
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    text_input(id)
        .state(state.downgrade())
        .placeholder(placeholder)
        .placeholder_color(theme.faint)
        .caret_color(theme.focus)
        .caret_blink_interval_500ms()
        .selection_color(with_alpha(theme.focus, 0.35))
        .text_sm()
        .text_color(theme.ink)
        .w_full()
        .min_h_auto()
        .whitespace_nowrap()
        .overflow_x_scroll()
        .px_2p5()
        .py_1p5()
        .rounded_md()
        .bg(theme.canvas)
        .border_1()
        .border_color(if focused { theme.focus } else { theme.line_strong })
}
