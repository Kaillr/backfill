mod calendar;
mod field;
mod motion;
mod outlook;
mod picker;
mod script;
mod settings_panel;
mod sources;
mod store;
mod theme;

use gpui::{
    App, AppContext, Bounds, QuitMode, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};
use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};

fn main() {
    // First, before anything else: the installer and the updater run backfill to finish their
    // work, and Velopack handles that here. A downloaded update waits for the Restart to
    // update button, or for backfill to quit, rather than restarting it as soon as it opens.
    velopack::VelopackApp::build().set_auto_apply_on_startup(false).run();
    gpui_platform::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx: &mut App| {
            cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
            calendar::bind_keys(cx);
            // After the text field's defaults, so these win inside fields.
            picker::bind_keys(cx);
            settings_panel::bind_keys(cx);

            let bounds = Bounds::centered(None, size(px(1480.), px(920.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // On macOS the header doubles as the titlebar and the app handles dragging
                    // it. Elsewhere the system titlebar stays.
                    titlebar: Some(TitlebarOptions {
                        title: Some("backfill".into()),
                        appears_transparent: cfg!(target_os = "macos"),
                        traffic_light_position: Some(point(px(18.), px(24.))),
                    }),
                    app_owns_titlebar_drag: cfg!(target_os = "macos"),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| calendar::Backfill::new(window, cx)),
            )
            .expect("the main window opens");
            cx.activate(true);
        });
}
