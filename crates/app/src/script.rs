//! A development aid: replay steps after launch so a state can be inspected or
//! screenshotted without clicking through it.
//!
//! `BACKFILL_SCRIPT="n f wait l type:104 enter"` selects the next unlogged item, fills the
//! day, then opens the picker and links issue 104. Only debug builds read it.
//!
//! Some steps go through the window like real input, so key bindings and text fields can
//! be checked: `key:cmd-a` presses keys, `text:hello~there` types (`~` is a space), and
//! `click:x,y` clicks at a point in the window, `rclick:x,y` right-clicks, and `drag:x,y,x,y` drags from one point to
//! another. `shot:name` runs `$BACKFILL_SHOT name pid`.

use std::time::Duration;

use gpui::{
    App, Context, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Task,
    Window, point, px,
};

use crate::{
    calendar::{
        ApplySync, Backfill, DeleteSelection, Dismiss, FillDay, Link, NextAttention,
        NextWeek, PrevWeek, ReviewSync, ToggleAbsorb, Undo,
    },
    picker,
};

pub fn start(window: &mut Window, cx: &mut Context<Backfill>) -> Option<Task<()>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let script = std::env::var("BACKFILL_SCRIPT").ok()?;
    let steps: Vec<String> = script.split_whitespace().map(str::to_owned).collect();
    Some(cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(900)).await;
        for step in steps {
            if let Some(name) = step.strip_prefix("shot:") {
                let Ok(command) = std::env::var("BACKFILL_SHOT") else { continue };
                let name = name.to_owned();
                cx.background_executor()
                    .spawn(async move { std::process::Command::new(command).arg(name).arg(std::process::id().to_string()).status() })
                    .await
                    .ok();
                continue;
            }
            let pause = if step == "wait" { 1200 } else { 500 };
            // Input goes to the window, outside any view's update, as real input would.
            let result = match cx.update(|window, cx| input(&step, window, cx)) {
                Ok(true) => Ok(()),
                _ => this.update_in(cx, |this, window, cx| run(this, &step, window, cx)),
            };
            if result.is_err() {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(pause)).await;
        }
    }))
}

/// Handles a step that is real input. Returns whether it was one.
fn input(step: &str, window: &mut Window, cx: &mut App) -> bool {
    if let Some(keys) = step.strip_prefix("key:") {
        for key in keys.split(',') {
            match Keystroke::parse(key) {
                Ok(keystroke) => _ = window.dispatch_keystroke(keystroke, cx),
                Err(_) => eprintln!("BACKFILL_SCRIPT: unknown key {key:?}"),
            }
        }
        return true;
    }
    if let Some(text) = step.strip_prefix("text:") {
        for c in text.replace('~', " ").chars() {
            let key = c.to_string();
            let keystroke = Keystroke { modifiers: Modifiers::default(), key: key.clone(), key_char: Some(key) };
            window.dispatch_keystroke(keystroke, cx);
        }
        return true;
    }
    if let Some(points) = step.strip_prefix("drag:") {
        let numbers: Vec<f32> = points.split(',').filter_map(|n| n.parse().ok()).collect();
        let [x1, y1, x2, y2] = numbers[..] else { return true };
        let (from, to) = (point(px(x1), px(y1)), point(px(x2), px(y2)));
        let modifiers = Modifiers::default();
        let button = MouseButton::Left;
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent { button, position: from, modifiers, click_count: 1, first_mouse: false }),
            cx,
        );
        for i in 1..=8 {
            let position = from + (to - from) * (i as f32 / 8.);
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(button), modifiers }), cx);
        }
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position: to, modifiers, click_count: 1 }), cx);
        return true;
    }
    let click = step.strip_prefix("click:").map(|p| (p, MouseButton::Left));
    let right_click = step.strip_prefix("rclick:").map(|p| (p, MouseButton::Right));
    if let Some((point_text, button)) = click.or(right_click)
        && let Some((x, y)) = point_text.split_once(',')
    {
        let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) else { return true };
        let position = point(px(x), px(y));
        let modifiers = Modifiers::default();
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers, click_count: 1, first_mouse: false }),
            cx,
        );
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers, click_count: 1 }), cx);
        return true;
    }
    false
}

fn run(this: &mut Backfill, step: &str, window: &mut Window, cx: &mut Context<Backfill>) {
    if let Some(picker) = this.open_picker_entity() {
        match step {
            "down" => return picker.update(cx, |p, cx| p.select_next(&picker::SelectNext, window, cx)),
            "up" => return picker.update(cx, |p, cx| p.select_prev(&picker::SelectPrev, window, cx)),
            "enter" => return picker.update(cx, |p, cx| p.confirm(&picker::Confirm, window, cx)),
            "esc" => return picker.update(cx, |p, cx| p.cancel(&picker::Cancel, window, cx)),
            _ => {}
        }
        if let Some(text) = step.strip_prefix("type:") {
            return picker.update(cx, |p, cx| p.set_query(text, cx));
        }
    }
    match step {
        "n" => this.next_attention(&NextAttention, window, cx),
        "f" => this.fill_day(&FillDay, window, cx),
        "l" => this.link(&Link, window, cx),
        "a" => this.toggle_absorb(&ToggleAbsorb, window, cx),
        "del" => this.delete_selection(&DeleteSelection, window, cx),
        "left" => this.prev_week(&PrevWeek, window, cx),
        "right" => this.next_week(&NextWeek, window, cx),
        "sync" => this.review_sync(&ReviewSync, window, cx),
        "apply" => this.apply_sync(&ApplySync, window, cx),
        "enter" => this.link(&Link, window, cx),
        "esc" => this.dismiss(&Dismiss, window, cx),
        "undo" => this.undo(&Undo, window, cx),
        "settings" => this.open_settings_action(&crate::calendar::OpenSettings, window, cx),
        "wait" => {}
        other => eprintln!("BACKFILL_SCRIPT: unknown step {other:?}"),
    }
}
