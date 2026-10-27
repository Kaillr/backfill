mod calendar;
mod field;
mod motion;
mod outlook;
mod picker;
mod script;
mod settings;
mod sources;
mod store;
mod theme;

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, OsAction, QuitMode, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowOptions, actions, point, px, size,
};
use gpui_elements::editable_text::actions::{self as text, DEFAULT_INPUT_CONTEXT, default_bindings};

actions!(app, [Quit, CheckForUpdates]);

/// The menu bar. Settings and Quit work from any window, with the usual shortcuts.
fn set_up_menus(cx: &mut App) {
    cx.on_action(|_: &calendar::OpenSettings, cx| calendar::open_settings(None, cx));
    cx.on_action(|_: &CheckForUpdates, cx| calendar::check_for_updates(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([KeyBinding::new("secondary-,", calendar::OpenSettings, None), KeyBinding::new("secondary-q", Quit, None)]);
    cx.set_menus([
        Menu::new("backfill").items([
            MenuItem::action("Settings…", calendar::OpenSettings),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit backfill", Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Cut", text::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", text::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", text::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", text::SelectAll, OsAction::SelectAll),
        ]),
    ]);
}

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
            settings::bind_keys(cx);
            set_up_menus(cx);

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
            calendar::open_settings_until_connected(cx);
            cx.activate(true);
        });
}
