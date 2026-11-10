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
        CopyMcpSetup,
        InstallAgentSkill,
        CopyAgentSetupPrompt,
        CopyForAi,
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
    cx.on_action(move |_: &CopyMcpSetup, cx| copy_mcp_setup(&s, cx));
    let s = state.clone();
    cx.on_action(move |_: &InstallAgentSkill, cx| install_agent_skill(&s, cx));
    let s = state.clone();
    cx.on_action(move |_: &CopyAgentSetupPrompt, cx| copy_agent_setup_prompt(&s, cx));
    let s = state.clone();
    cx.on_action(move |_: &CopyForAi, cx| crate::insights::copy_for_ai(s.clone(), cx));
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
        let key = (s.sort, s.descending, s.settings.graph_free, s.settings.deep_activity, s.screen, s.job.is_some(), s.location.is_some(), s.settings.show_sidebar, s.current.is_some());
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
            enabled(MenuItem::action("Copy Insights for AI", CopyForAi), s.current.is_some()),
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
    help.push(MenuItem::action("Install skill for AI agents", InstallAgentSkill));
    help.push(MenuItem::action("Copy setup prompt for AI agents", CopyAgentSetupPrompt));
    help.push(MenuItem::action("Copy MCP setup", CopyMcpSetup));
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

/// Copies the MCP client config that runs `diskdiff-cli mcp`; the tool ships
/// next to the app's executable (inside the bundle on macOS).
///
/// Linux AppImage: the app runs from a temporary mount that disappears when
/// it quits, so the config runs the `.AppImage` file itself with `cli mcp`,
/// which its AppRun hands to `diskdiff-cli` (see packaging/linux/AppRun).
fn copy_mcp_setup(state: &Entity<AppState>, cx: &mut App) {
    let (json, found) = match std::env::var_os("APPIMAGE").map(std::path::PathBuf::from) {
        Some(image) => (mcp_setup_json(&image, &["cli", "mcp"]), image.is_file()),
        None => {
            let (cli, found) = sibling_cli();
            (mcp_setup_json(&cli, &["mcp"]), found)
        }
    };
    cx.write_to_clipboard(ClipboardItem::new_string(json));
    let detail = if found {
        "Paste it into your MCP client’s config, e.g. Claude Desktop."
    } else {
        "diskdiff-cli wasn’t found next to the app, so the path may need fixing."
    };
    state.update(cx, |s, cx| s.notify_toast("MCP setup copied", Some(detail.into()), cx));
}

/// How an agent's shell runs `diskdiff-cli`: its path next to the app,
/// quoted, or the AppImage it was started from plus `cli` (the AppImage's
/// mount is temporary). And whether that file is there.
fn cli_command() -> (String, bool) {
    match std::env::var_os("APPIMAGE").map(std::path::PathBuf::from) {
        Some(image) => (format!("\"{}\" cli", image.display()), image.is_file()),
        None => {
            let (cli, found) = sibling_cli();
            (format!("\"{}\"", cli.display()), found)
        }
    }
}

/// Saves a skill that tells AI agents to run `diskdiff-cli` themselves when
/// asked about disk space: in `~/.claude/skills` for Claude Code and in
/// `~/.agents/skills`, the shared folder of Codex, Cursor, Gemini CLI and
/// GitHub Copilot.
fn install_agent_skill(state: &Entity<AppState>, cx: &mut App) {
    let (command, found) = cli_command();
    let result = match dirs::home_dir() {
        Some(home) => write_skill(&home, &command),
        None => Err(std::io::Error::other("no home folder")),
    };
    state.update(cx, |s, cx| match result {
        Ok(_) => {
            let detail = if found {
                "Claude Code, Codex, Cursor, Gemini CLI and Copilot now use DiskDiff when you ask about disk space. Saved to ~/.claude/skills and ~/.agents/skills."
            } else {
                "Saved to ~/.claude/skills and ~/.agents/skills, but diskdiff-cli wasn’t found next to the app, so the path in it may need fixing."
            };
            s.notify_toast("Skill installed", Some(detail.into()), cx)
        }
        Err(e) => s.notify_toast("Could not install the skill", Some(e.to_string().into()), cx),
    });
}

/// Copies a prompt that asks any AI agent to set DiskDiff up for itself, for
/// agents whose skills live elsewhere or that run on another setup.
fn copy_agent_setup_prompt(state: &Entity<AppState>, cx: &mut App) {
    let (command, found) = cli_command();
    cx.write_to_clipboard(ClipboardItem::new_string(diskdiff_cli::report::setup_prompt(&command)));
    let detail = if found {
        "Paste it into your AI agent, and it sets DiskDiff up for itself."
    } else {
        "diskdiff-cli wasn’t found next to the app, so the path in it may need fixing."
    };
    state.update(cx, |s, cx| s.notify_toast("Setup prompt copied", Some(detail.into()), cx));
}

/// Writes the skill for `command` to `diskdiff/SKILL.md` under
/// `<home>/.claude/skills` and `<home>/.agents/skills`, replacing earlier
/// ones (the files are DiskDiff's), and returns their paths.
fn write_skill(home: &std::path::Path, command: &str) -> std::io::Result<Vec<std::path::PathBuf>> {
    let text = diskdiff_cli::report::skill(command);
    [".claude", ".agents"]
        .iter()
        .map(|agent| {
            let dir = home.join(agent).join("skills").join("diskdiff");
            std::fs::create_dir_all(&dir)?;
            let file = dir.join("SKILL.md");
            std::fs::write(&file, &text)?;
            Ok(file)
        })
        .collect()
}

/// `diskdiff-cli` next to the running app, and whether it's there (it isn't
/// in a plain `cargo run` until it's built too).
fn sibling_cli() -> (std::path::PathBuf, bool) {
    let cli = std::env::current_exe().ok().map(|exe| cli_path(&exe));
    let found = cli.as_ref().is_some_and(|p| p.is_file());
    (cli.unwrap_or_else(|| cli_name().into()), found)
}

fn cli_name() -> &'static str {
    if cfg!(windows) { "diskdiff-cli.exe" } else { "diskdiff-cli" }
}

fn cli_path(exe: &std::path::Path) -> std::path::PathBuf {
    exe.with_file_name(cli_name())
}

/// `{"mcpServers":{"diskdiff":{"command":"…","args":[…]}}}`, pretty-printed
/// for pasting.
fn mcp_setup_json(command: &std::path::Path, args: &[&str]) -> String {
    let entry = serde_json::json!({ "command": command.to_string_lossy(), "args": args });
    let config = serde_json::json!({ "mcpServers": { "diskdiff": entry } });
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
    fn mcp_setup_names_the_sibling_cli() {
        let exe = Path::new("/Applications/DiskDiff.app/Contents/MacOS/diskdiff");
        let cli = cli_path(exe);
        assert_eq!(cli.parent(), exe.parent());
        assert_eq!(cli.file_name().unwrap(), cli_name());

        let json: serde_json::Value =
            serde_json::from_str(&mcp_setup_json(Path::new("/opt/app/diskdiff-cli"), &["mcp"])).unwrap();
        assert_eq!(json["mcpServers"]["diskdiff"]["command"], "/opt/app/diskdiff-cli");
        assert_eq!(json["mcpServers"]["diskdiff"]["args"], serde_json::json!(["mcp"]));

        // An AppImage runs the image file itself, which hands `cli mcp` on.
        let json: serde_json::Value =
            serde_json::from_str(&mcp_setup_json(Path::new("/opt/apps/DiskDiff.AppImage"), &["cli", "mcp"])).unwrap();
        assert_eq!(json["mcpServers"]["diskdiff"]["command"], "/opt/apps/DiskDiff.AppImage");
        assert_eq!(json["mcpServers"]["diskdiff"]["args"], serde_json::json!(["cli", "mcp"]));
    }

    #[test]
    fn skill_lands_in_both_skill_folders() {
        let home = std::env::temp_dir().join(format!("diskdiff-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let files = write_skill(&home, "\"/opt/app/diskdiff-cli\"").unwrap();
        let claude = home.join(".claude").join("skills").join("diskdiff").join("SKILL.md");
        let agents = home.join(".agents").join("skills").join("diskdiff").join("SKILL.md");
        assert_eq!(files, vec![claude.clone(), agents.clone()]);
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            assert!(text.starts_with("---\nname: diskdiff\n"), "{text}");
            assert!(text.contains("\"/opt/app/diskdiff-cli\" call list_scans"), "{text}");
        }

        // A second install replaces the first.
        write_skill(&home, "\"/opt/apps/DiskDiff.AppImage\" cli").unwrap();
        for file in [&claude, &agents] {
            let text = std::fs::read_to_string(file).unwrap();
            assert!(text.contains("\"/opt/apps/DiskDiff.AppImage\" cli call list_scans"), "{text}");
            assert!(!text.contains("/opt/app/diskdiff-cli"), "{text}");
        }
        std::fs::remove_dir_all(&home).unwrap();
    }
}
