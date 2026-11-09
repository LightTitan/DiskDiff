#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! DiskDiff: a disk analyzer that shows how folder sizes change between scans.

mod browser;
mod control;
mod dev;
mod diskbar;
mod format;
mod graph;
mod insights;
mod manage;
mod menus;
mod services;
mod settings;
mod sidebar;
mod state;
mod summary;
mod theme;
mod widgets;
mod workspace;

use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;
use std::sync::Arc;

fn main() {
    application().with_assets(assets::AllAssets).run(|cx| {
        init(cx);
        theme::init(cx);

        // DISKDIFF_DB points at another store (demo data, tests).
        let db = diskdiff_engine::platform::store_path();
        let store = match diskdiff_engine::Store::open(&db) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!("could not open {}: {e:#}", db.display());
                std::process::exit(1);
            }
        };
        let state = cx.new(|cx| state::AppState::new(store, cx));
        let state2 = state.clone();
        menus::init(state.clone(), cx);
        services::init(state.clone(), cx);
        control::init(state.clone(), cx);
        // Temporary scans never outlive the session.
        let store_for_quit = state.read(cx).store.clone();
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.on_app_quit(move |_| {
            control::shutdown();
            let store = store_for_quit.clone();
            async move {
                if let Ok(ids) = store.temporary_ids() {
                    let _ = store.delete_scans(&ids);
                }
            }
        })
        .detach();

        // Open already filling the screen. `WindowBounds::Maximized` zooms
        // after the window appears, which plays the zoom animation.
        let bounds = cx
            .primary_display()
            .map(|d| d.visible_bounds())
            .unwrap_or_else(|| Bounds::centered(None, size(px(1200.), px(820.)), cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.), px(480.))),
            // Matches StartupWMClass in packaging/linux/diskdiff.desktop, so
            // Linux docks group the window under the installed launcher.
            app_id: Some("diskdiff".into()),
            ..TitleBar::window_options()
        };
        let window = cx.open_window(options, |window, cx| {
            theme::apply_appearance(window.appearance(), Some(window), cx);
            window
                .observe_window_appearance(|window, cx| theme::apply_appearance(window.appearance(), Some(window), cx))
                .detach();
            let view = cx.new(|cx| workspace::Workspace::new(state2, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("open window");
        cx.activate(true);
        dev::hooks(window, state, cx);
    });
}
