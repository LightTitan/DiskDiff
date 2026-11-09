//! The menu bar: on macOS the standard DiskDiff / File / Edit / View / Go /
//! Window / Help menus with their usual shortcuts. Windows and Linux have no
//! system menu bar, so the same menus (minus the macOS-only ones) are drawn
//! in the window's title bar by [`MenuBar`].
//!
//! App-wide actions are handled here, on the app. Browser actions (Go menu,
//! Copy) are handled by the browser itself, so those items are only enabled
//! while it has focus; GPUI greys out items whose action nothing handles.

use crate::browser;
use crate::state::{AppState, InsightsTab, Screen, SortKey};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

/// The in-window menu bar on Windows and Linux, rebuilt whenever the menus
/// change. Unused on macOS, where the system draws the menus.
pub struct MenuBar(pub Entity<gpui_kit::component::menu::AppMenuBar>);

impl Global for MenuBar {}

impl MenuBar {
    pub fn get(cx: &mut App) -> Entity<gpui_kit::component::menu::AppMenuBar> {
        if let Some(bar) = cx.try_global::<MenuBar>() {
            return bar.0.clone();
        }
        let bar = gpui_kit::component::menu::AppMenuBar::new(cx);
        cx.set_global(MenuBar(bar.clone()));
        bar
    }
}

actions!(
    diskdiff,
    [
        Quit,
        About,
        Hide,
        HideOthers,
        ShowAll,
        ScanNow,
        TemporaryScan,
        AddLocation,
        ManageScans,
        CancelScan,
        CloseWindow,
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        SelectAll,
        ShowFiles,
        ShowSuggestions,
        ShowLargest,
        ShowChanges,
        SortBySize,
        SortByChange,
        SortByChangeSigned,
        SortByName,
        ReverseSortOrder,
        ToggleGraphFree,
        ToggleDeepActivity,
        ToggleFullScreen,
        ToggleSidebar,
        Minimize,
        Zoom,
        BringAllToFront,
        OpenPrivacySettings,
        ShowDatabase,
        CopyAgentSetup,
    ]
);

pub fn init(state: Entity<AppState>, cx: &mut App) {
    // `secondary` is Cmd on macOS and Ctrl elsewhere.
    cx.bind_keys([
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-n", ScanNow, None),
        KeyBinding::new("secondary-shift-n", TemporaryScan, None),
        KeyBinding::new("secondary-o", AddLocation, None),
        KeyBinding::new("secondary-shift-m", ManageScans, None),
        KeyBinding::new("secondary-.", CancelScan, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("secondary-z", Undo, None),
        KeyBinding::new("secondary-shift-z", Redo, None),
        KeyBinding::new("secondary-x", Cut, None),
        KeyBinding::new("secondary-c", Copy, None),
        KeyBinding::new("secondary-v", Paste, None),
        KeyBinding::new("secondary-a", SelectAll, None),
        KeyBinding::new("secondary-1", ShowFiles, None),
        KeyBinding::new("secondary-2", ShowSuggestions, None),
        KeyBinding::new("secondary-3", ShowLargest, None),
        KeyBinding::new("secondary-4", ShowChanges, None),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("cmd-alt-h", HideOthers, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
            KeyBinding::new("ctrl-cmd-s", ToggleSidebar, None),
            KeyBinding::new("cmd-m", Minimize, None),
        ]);
    } else {
        cx.bind_keys([
            KeyBinding::new("ctrl-y", Redo, None),
            KeyBinding::new("f11", ToggleFullScreen, None),
            KeyBinding::new("ctrl-shift-s", ToggleSidebar, None),
        ]);
    }

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &OpenPrivacySettings, cx| {
        cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
    });
    cx.on_action(|_: &CloseWindow, cx| with_window(cx, |w, _| w.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_window(cx, |w, _| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_window(cx, |w, _| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_window(cx, |w, _| w.toggle_fullscreen()));
    cx.on_action(|_: &About, cx| with_window(cx, about));

    let on = |f: fn(&mut AppState, &mut Context<AppState>)| {
        let state = state.clone();
        move |cx: &mut App| state.update(cx, |s, cx| f(s, cx))
    };
    let f = on(|s, cx| {
        if let Some(loc) = s.location.clone() {
            s.start_scan(loc, false, cx)
        }
    });
    cx.on_action(move |_: &ScanNow, cx| f(cx));
    let f = on(|s, cx| s.cancel_scan(cx));
    cx.on_action(move |_: &CancelScan, cx| f(cx));
    let f = on(|s, cx| {
        s.show_manage = true;
        cx.notify();
    });
    cx.on_action(move |_: &ManageScans, cx| f(cx));
    let f = on(|s, cx| s.set_screen(Screen::Files, cx));
    cx.on_action(move |_: &ShowFiles, cx| f(cx));
    let f = on(|s, cx| s.set_screen(Screen::Insights(InsightsTab::Suggestions), cx));
    cx.on_action(move |_: &ShowSuggestions, cx| f(cx));
    let f = on(|s, cx| s.set_screen(Screen::Insights(InsightsTab::Largest), cx));
    cx.on_action(move |_: &ShowLargest, cx| f(cx));
    let f = on(|s, cx| s.set_screen(Screen::Insights(InsightsTab::Changes), cx));
    cx.on_action(move |_: &ShowChanges, cx| f(cx));
    let f = on(|s, cx| s.set_sort(SortKey::Size, cx));
    cx.on_action(move |_: &SortBySize, cx| f(cx));
    let f = on(|s, cx| s.set_sort(SortKey::Delta, cx));
    cx.on_action(move |_: &SortByChange, cx| f(cx));
    let f = on(|s, cx| s.set_sort(SortKey::DeltaSigned, cx));
    cx.on_action(move |_: &SortByChangeSigned, cx| f(cx));
    let f = on(|s, cx| s.set_sort(SortKey::Name, cx));
    cx.on_action(move |_: &SortByName, cx| f(cx));
    let f = on(|s, cx| s.toggle_order(cx));
    cx.on_action(move |_: &ReverseSortOrder, cx| f(cx));
    let f = on(|s, cx| {
        let free = !s.settings.graph_free;
        s.set_graph_free(free, cx)
    });
    cx.on_action(move |_: &ToggleGraphFree, cx| f(cx));
    let f = on(|s, cx| {
        let deep = !s.settings.deep_activity;
        s.set_deep_activity(deep, cx)
    });
    cx.on_action(move |_: &ToggleDeepActivity, cx| f(cx));
    let f = on(|s, cx| {
        let show = !s.settings.show_sidebar;
        s.set_sidebar(show, cx)
    });
    cx.on_action(move |_: &ToggleSidebar, cx| f(cx));
    let s = state.clone();
    cx.on_action(move |_: &ShowDatabase, cx| {
        diskdiff_engine::platform::reveal(s.read(cx).store.path());
    });
    let s = state.clone();
    cx.on_action(move |_: &CopyAgentSetup, cx| copy_agent_setup(&s, cx));
    let s = state.clone();
    cx.on_action(move |_: &AddLocation, cx| crate::workspace::pick_folder(s.clone(), false, cx));
    let s = state.clone();
    cx.on_action(move |_: &TemporaryScan, cx| crate::workspace::pick_folder(s.clone(), true, cx));

    // Checkmarks follow the state; rebuild only when they would change.
    let mut last = None;
    let initial = menus(state.read(cx));
    set_menus(initial, cx);
    cx.observe(&state, move |state, cx| {
        let s = state.read(cx);
        let key = (s.sort, s.descending, s.settings.graph_free, s.settings.deep_activity, s.screen, s.job.is_some(), s.location.is_some(), s.settings.show_sidebar);
        if last != Some(key) {
            last = Some(key);
            let next = menus(s);
            set_menus(next, cx);
        }
    })
    .detach();
}

/// Runs `f` on our window. Deferred: actions are dispatched while the window
/// is being updated, and a nested update of the same window is refused.
fn with_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    cx.defer(move |cx| {
        // The key window, or our only window when the app is in the background.
        if let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) {
            let _ = w.update(cx, |_, window, cx| f(window, cx));
        }
    });
}

fn checked(name: &'static str, action: impl Action, on: bool) -> MenuItem {
    match MenuItem::action(name, action) {
        MenuItem::Action { name, action, os_action, disabled, .. } => {
            MenuItem::Action { name, action, os_action, checked: on, disabled }
        }
        other => other,
    }
}

fn enabled(item: MenuItem, on: bool) -> MenuItem {
    match item {
        MenuItem::Action { name, action, os_action, checked, .. } => {
            MenuItem::Action { name, action, os_action, checked, disabled: !on }
        }
        other => other,
    }
}

fn set_menus(menus: Vec<Menu>, cx: &mut App) {
    cx.set_menus(menus);
    if !cfg!(target_os = "macos") {
        let menus = cx.get_menus().unwrap_or_default();
        gpui_kit::component::GlobalState::global_mut(cx).set_app_menus(menus);
        if let Some(bar) = cx.try_global::<MenuBar>().map(|b| b.0.clone()) {
            bar.update(cx, |bar, cx| bar.reload(cx));
        }
    }
}

fn menus(s: &AppState) -> Vec<Menu> {
    let mac = cfg!(target_os = "macos");
    let scanning = s.job.is_some();
    let screen = s.screen;
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu::new("DiskDiff").items([
            MenuItem::action("About DiskDiff", About),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide DiskDiff", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit DiskDiff", Quit),
        ]));
    }
    let mut file = vec![
            enabled(MenuItem::action("Scan Now", ScanNow), !scanning && s.location.is_some()),
            enabled(MenuItem::action("Temporary Scan…", TemporaryScan), !scanning),
            MenuItem::action("Add Location…", AddLocation),
            MenuItem::separator(),
            enabled(MenuItem::action("Cancel Scan", CancelScan), scanning),
            MenuItem::separator(),
            MenuItem::action("Add to Sidebar", browser::AddToSidebar),
            MenuItem::separator(),
            MenuItem::action("Manage Scans…", ManageScans),
            MenuItem::action(format!("Show Database in {}", file_manager_title()), ShowDatabase),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
    ];
    if !mac {
        // No app menu off macOS: Quit lives at the end of File, as usual there.
        file.push(MenuItem::action("Quit", Quit));
    }
    menus.push(Menu::new("File").items(file));
    menus.push(Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Copy Path", browser::CopyPath),
        ]));
    menus.push(Menu::new("View").items([
            MenuItem::action(if s.settings.show_sidebar { "Hide Sidebar" } else { "Show Sidebar" }, ToggleSidebar),
            MenuItem::separator(),
            checked("Files", ShowFiles, screen == Screen::Files),
            checked("Suggestions", ShowSuggestions, screen == Screen::Insights(InsightsTab::Suggestions)),
            checked("Largest Items", ShowLargest, screen == Screen::Insights(InsightsTab::Largest)),
            checked("Changes", ShowChanges, screen == Screen::Insights(InsightsTab::Changes)),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Sort By").items([
                checked("Size", SortBySize, s.sort == SortKey::Size),
                checked("Change", SortByChange, s.sort == SortKey::Delta),
                checked("Change +/−", SortByChangeSigned, s.sort == SortKey::DeltaSigned),
                checked("Name", SortByName, s.sort == SortKey::Name),
                MenuItem::separator(),
                checked("Descending", ReverseSortOrder, s.descending),
            ])),
            checked("Graph Available Space", ToggleGraphFree, s.settings.graph_free),
            checked("Deep Folder Activity", ToggleDeepActivity, s.settings.deep_activity),
            MenuItem::separator(),
            MenuItem::action(if mac { "Enter Full Screen" } else { "Full Screen" }, ToggleFullScreen),
        ]));
    menus.push(Menu::new("Go").items([
            MenuItem::action("Enclosing Folder", browser::GoParent),
            MenuItem::action("Open", browser::OpenItem),
            MenuItem::action(format!("Show in {}", file_manager_title()), browser::Reveal),
        ]));
    if mac {
        menus.push(Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", BringAllToFront),
        ]));
    }
    let mut help = Vec::new();
    if mac {
        help.push(MenuItem::action("Full Disk Access Settings…", OpenPrivacySettings));
        help.push(MenuItem::separator());
    }
    help.push(MenuItem::action("Copy AI agent setup", CopyAgentSetup));
    if !mac {
        help.push(MenuItem::separator());
        help.push(MenuItem::action("About DiskDiff", About));
    }
    menus.push(Menu::new("Help").items(help));
    menus
}

/// The file manager's name for a menu title: "Finder", "File Explorer",
/// "File Manager".
fn file_manager_title() -> &'static str {
    match crate::state::file_manager() {
        "file manager" => "File Manager",
        name => name,
    }
}

/// Copies the MCP client config that runs `diskdiff-mcp`, which ships next
/// to the app's executable (inside the bundle on macOS).
fn copy_agent_setup(state: &Entity<AppState>, cx: &mut App) {
    let server = std::env::current_exe().ok().map(|exe| mcp_server_path(&exe));
    let found = server.as_ref().is_some_and(|p| p.is_file());
    let server = server.unwrap_or_else(|| mcp_server_name().into());
    cx.write_to_clipboard(ClipboardItem::new_string(agent_setup_json(&server)));
    let detail = if found {
        "Paste it into your MCP client’s config, e.g. Claude Desktop."
    } else {
        "The diskdiff-mcp server wasn’t found next to the app, so the path may need fixing."
    };
    state.update(cx, |s, cx| s.notify_toast("Agent setup copied", Some(detail.into()), cx));
}

fn mcp_server_name() -> &'static str {
    if cfg!(windows) { "diskdiff-mcp.exe" } else { "diskdiff-mcp" }
}

fn mcp_server_path(exe: &std::path::Path) -> std::path::PathBuf {
    exe.with_file_name(mcp_server_name())
}

/// `{"mcpServers":{"diskdiff":{"command":"…"}}}`, pretty-printed for pasting.
fn agent_setup_json(server: &std::path::Path) -> String {
    let config = serde_json::json!({ "mcpServers": { "diskdiff": { "command": server.to_string_lossy() } } });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

fn about(window: &mut Window, cx: &mut App) {
    let version = env!("CARGO_PKG_VERSION");
    window.open_dialog(cx, move |dialog, _, cx| {
        let p = crate::theme::ActivePalette::palette(cx);
        dialog.width(px(340.)).child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .py(px(8.))
                .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child("DiskDiff"))
                .child(div().text_size(px(12.)).text_color(p.dim).child(format!("Version {version}")))
                .child(
                    div()
                        .pt(px(6.))
                        .text_size(px(12.))
                        .text_color(p.dim)
                        .text_center()
                        .child("See what grew, what shrank, and what changed between scans of your disk."),
                ),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings its own `test` macro; use the standard one.
    use core::prelude::v1::test;
    use std::path::Path;

    #[test]
    fn agent_setup_names_the_sibling_server() {
        let exe = Path::new("/Applications/DiskDiff.app/Contents/MacOS/diskdiff");
        let server = mcp_server_path(exe);
        assert_eq!(server.parent(), exe.parent());
        assert_eq!(server.file_name().unwrap(), mcp_server_name());

        let json: serde_json::Value = serde_json::from_str(&agent_setup_json(Path::new("/opt/app/diskdiff-mcp"))).unwrap();
        assert_eq!(json["mcpServers"]["diskdiff"]["command"], "/opt/app/diskdiff-mcp");
    }
}
