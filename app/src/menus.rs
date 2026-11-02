//! The macOS menu bar: the standard DiskDiff / File / Edit / View / Go /
//! Window / Help menus with their usual shortcuts.
//!
//! App-wide actions are handled here, on the app. Browser actions (Go menu,
//! Copy) are handled by the browser itself, so those items are only enabled
//! while it has focus; GPUI greys out items whose action nothing handles.

use crate::browser;
use crate::state::{AppState, InsightsTab, Screen, SortKey};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

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
    ]
);

pub fn init(state: Entity<AppState>, cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-n", ScanNow, None),
        KeyBinding::new("cmd-shift-n", TemporaryScan, None),
        KeyBinding::new("cmd-o", AddLocation, None),
        KeyBinding::new("cmd-shift-m", ManageScans, None),
        KeyBinding::new("cmd-.", CancelScan, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("cmd-1", ShowFiles, None),
        KeyBinding::new("cmd-2", ShowSuggestions, None),
        KeyBinding::new("cmd-3", ShowLargest, None),
        KeyBinding::new("cmd-4", ShowChanges, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("ctrl-cmd-s", ToggleSidebar, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);

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
    cx.on_action(move |_: &AddLocation, cx| crate::workspace::pick_folder(s.clone(), false, cx));
    let s = state.clone();
    cx.on_action(move |_: &TemporaryScan, cx| crate::workspace::pick_folder(s.clone(), true, cx));

    // Checkmarks follow the state; rebuild only when they would change.
    let mut last = None;
    set_menus(state.read(cx), cx);
    cx.observe(&state, move |state, cx| {
        let s = state.read(cx);
        let key = (s.sort, s.descending, s.settings.graph_free, s.settings.deep_activity, s.screen, s.job.is_some(), s.location.is_some(), s.settings.show_sidebar);
        if last != Some(key) {
            last = Some(key);
            set_menus(s, cx);
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

fn set_menus(s: &AppState, cx: &App) {
    let scanning = s.job.is_some();
    let screen = s.screen;
    cx.set_menus([
        Menu::new("DiskDiff").items([
            MenuItem::action("About DiskDiff", About),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide DiskDiff", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit DiskDiff", Quit),
        ]),
        Menu::new("File").items([
            enabled(MenuItem::action("Scan Now", ScanNow), !scanning && s.location.is_some()),
            enabled(MenuItem::action("Temporary Scan…", TemporaryScan), !scanning),
            MenuItem::action("Add Location…", AddLocation),
            MenuItem::separator(),
            enabled(MenuItem::action("Cancel Scan", CancelScan), scanning),
            MenuItem::separator(),
            MenuItem::action("Add to Sidebar", browser::AddToSidebar),
            MenuItem::separator(),
            MenuItem::action("Manage Scans…", ManageScans),
            MenuItem::action("Show Database in Finder", ShowDatabase),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Copy Path", browser::CopyPath),
        ]),
        Menu::new("View").items([
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
            checked("Graph Free Space", ToggleGraphFree, s.settings.graph_free),
            checked("Deep Folder Activity", ToggleDeepActivity, s.settings.deep_activity),
            MenuItem::separator(),
            MenuItem::action("Enter Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Enclosing Folder", browser::GoParent),
            MenuItem::action("Open", browser::OpenItem),
            MenuItem::action(
                if cfg!(windows) { "Show in File Explorer" } else { "Show in Finder" },
                browser::Reveal,
            ),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", BringAllToFront),
        ]),
        Menu::new("Help").items([MenuItem::action("Full Disk Access Settings…", OpenPrivacySettings)]),
    ]);
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
