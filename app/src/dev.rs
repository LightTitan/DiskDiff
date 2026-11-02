//! Development hooks driven by environment variables, for screenshots
//! without Screen Recording permission:
//!
//! * `DISKDIFF_SCREEN=insights|largest|changes|manage` opens that screen.
//! * `DISKDIFF_REVEAL=/abs/path` reveals a path in the browser.
//! * `DISKDIFF_SCAN=/abs/path` starts a temporary scan of that folder.
//! * `DISKDIFF_KEYS="down down right l i"` types keystrokes after 1 s
//!   (space-separated, GPUI keystroke syntax).
//! * `DISKDIFF_ACTION=diskdiff::About` dispatches an action by name.
//! * `DISKDIFF_SCROLL="500,600:-2,-15*20"` replays scroll gestures (see code).
//! * `DISKDIFF_PRINT_CLOUD=/abs/path` prints live cloud sizes and quits.
//! * `DISKDIFF_MOUSE="d:40,520 m:40,560 u:40,560"` replays mouse steps (see code).
//! * `DISKDIFF_PRINT_MENUS=1` prints the menu bar.
//! * `DISKDIFF_SCREENSHOT=out.png` saves the window after 2.5 s and quits
//!   (debug builds only: GPUI reads the frame back via `test-support`).

use crate::state::{AppState, InsightsTab, Screen};
use gpui_kit::*;
use std::time::Duration;

pub fn hooks<V: 'static>(window: WindowHandle<V>, state: Entity<AppState>, cx: &mut App) {
    if let Ok(screen) = std::env::var("DISKDIFF_SCREEN") {
        state.update(cx, |s, cx| match screen.as_str() {
            "insights" => s.set_screen(Screen::Insights(InsightsTab::Suggestions), cx),
            "largest" => s.set_screen(Screen::Insights(InsightsTab::Largest), cx),
            "changes" => s.set_screen(Screen::Insights(InsightsTab::Changes), cx),
            "manage" => {
                s.show_manage = true;
                cx.notify();
            }
            _ => {}
        });
    }
    if let Ok(path) = std::env::var("DISKDIFF_SCAN") {
        state.update(cx, |s, cx| s.start_scan(path.into(), true, cx));
    }
    if let Ok(path) = std::env::var("DISKDIFF_REVEAL") {
        state.update(cx, |s, cx| s.go_to(path.into(), cx));
    }
    if let Ok(keys) = std::env::var("DISKDIFF_KEYS") {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(1000)).await;
            for k in keys.split_whitespace() {
                let Ok(mut ks) = Keystroke::parse(k) else { continue };
                if ks.key.chars().count() == 1 && ks.key_char.is_none() && !ks.modifiers.platform {
                    ks.key_char = Some(ks.key.clone());
                }
                let _ = cx.update(|cx| {
                    let _ = window.update(cx, |_, window, cx| window.dispatch_keystroke(ks, cx));
                });
                cx.background_executor().timer(Duration::from_millis(60)).await;
            }
        })
        .detach();
    }
    if let Ok(name) = std::env::var("DISKDIFF_ACTION") {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            let _ = cx.update(|cx| match cx.build_action(&name, None) {
                Ok(action) => {
                    let _ = window.update(cx, |_, window, cx| window.dispatch_action(action, cx));
                }
                Err(e) => eprintln!("unknown action {name}: {e}"),
            });
        })
        .detach();
    }
    if let Ok(spec) = std::env::var("DISKDIFF_SCROLL") {
        // "x,y:dx,dy*n" — one trackpad gesture at window point (x, y): n
        // events of (dx, dy) pixels, the first marked Started.
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(1200)).await;
            for gesture in spec.split(';') {
                let Some((at, step)) = gesture.split_once(':') else { continue };
                // Prefixes on the step: `L` = mouse wheel lines, `S` = Shift held.
                let lines = step.contains('L');
                let shift = step.contains('S');
                let step = step.trim_start_matches(|c| c == 'L' || c == 'S');
                let nums = |t: &str| -> Vec<f32> { t.split(|c| c == ',' || c == '*').filter_map(|v| v.parse().ok()).collect() };
                let (a, d) = (nums(at), nums(step));
                if a.len() < 2 || d.len() < 3 {
                    continue;
                }
                for i in 0..d[2] as usize {
                    let event = ScrollWheelEvent {
                        position: point(px(a[0]), px(a[1])),
                        delta: if lines {
                            ScrollDelta::Lines(point(d[0], d[1]))
                        } else {
                            ScrollDelta::Pixels(point(px(d[0]), px(d[1])))
                        },
                        modifiers: Modifiers { shift, ..Default::default() },
                        touch_phase: if i == 0 && !lines { TouchPhase::Started } else { TouchPhase::Moved },
                    };
                    let _ = cx.update(|cx| {
                        let _ = window.update(cx, |_, window, cx| window.dispatch_event(event.to_platform_input(), cx));
                    });
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                }
                cx.background_executor().timer(Duration::from_millis(400)).await;
            }
        })
        .detach();
    }
    if let Ok(path) = std::env::var("DISKDIFF_PRINT_CLOUD") {
        // Prints live cloud sizes of a folder's entries, then quits.
        let never = std::sync::atomic::AtomicBool::new(false);
        let t = std::time::Instant::now();
        let mut sizes: Vec<_> = diskdiff_engine::cloud::measure_children(path.as_ref(), &never).into_iter().collect();
        sizes.sort_by(|a, b| b.1.logical.cmp(&a.1.logical));
        for (name, c) in sizes {
            println!("{name}: total {} on disk {} partial {}", c.logical, c.on_disk, c.partial);
        }
        println!("took {:?}", t.elapsed());
        cx.quit();
    }
    if let Ok(spec) = std::env::var("DISKDIFF_MOUSE") {
        // Space-separated steps in window points: `d:x,y` press, `m:x,y`
        // move with the button held, `h:x,y` hover, `u:x,y` release,
        // `c:x,y` click, `w:ms` wait.
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            for step in spec.split_whitespace() {
                let Some((op, arg)) = step.split_once(':') else { continue };
                let n: Vec<f32> = arg.split(',').filter_map(|v| v.parse().ok()).collect();
                if op == "w" {
                    cx.background_executor().timer(Duration::from_millis(n.first().copied().unwrap_or(100.) as u64)).await;
                    continue;
                }
                if n.len() < 2 {
                    continue;
                }
                let at = point(px(n[0]), px(n[1]));
                let left = MouseButton::Left;
                let mods = Modifiers::default();
                let down = || MouseDownEvent { button: left, position: at, modifiers: mods, click_count: 1, first_mouse: false }.to_platform_input();
                let up = || MouseUpEvent { button: left, position: at, modifiers: mods, click_count: 1 }.to_platform_input();
                let moved = |held| MouseMoveEvent { position: at, pressed_button: held, modifiers: mods }.to_platform_input();
                let events = match op {
                    "d" => vec![down()],
                    "u" => vec![up()],
                    "m" => vec![moved(Some(left))],
                    "h" => vec![moved(None)],
                    "c" => vec![moved(None), down(), up()],
                    _ => vec![],
                };
                for e in events {
                    let _ = cx.update(|cx| {
                        let _ = window.update(cx, |_, window, cx| window.dispatch_event(e, cx));
                    });
                    cx.background_executor().timer(Duration::from_millis(30)).await;
                }
            }
        })
        .detach();
    }
    if std::env::var_os("DISKDIFF_PRINT_MENUS").is_some() {
        for m in cx.get_menus().unwrap_or_default() {
            let items: Vec<String> = m
                .items
                .iter()
                .map(|i| match i {
                    OwnedMenuItem::Action { name, checked, disabled, .. } => {
                        format!("{name}{}{}", if *checked { " ✓" } else { "" }, if *disabled { " (off)" } else { "" })
                    }
                    OwnedMenuItem::Submenu(s) => format!("{} ▸", s.name),
                    OwnedMenuItem::SystemMenu(s) => format!("{} ▸", s.name),
                    OwnedMenuItem::Separator => "—".into(),
                })
                .collect();
            println!("{}: {}", m.name, items.join(" | "));
        }
    }
    let Ok(out) = std::env::var("DISKDIFF_SCREENSHOT") else { return };
    cx.spawn(async move |cx| {
        let wait = std::env::var("DISKDIFF_SCREENSHOT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
        cx.background_executor().timer(Duration::from_millis(wait)).await;
        let _ = cx.update(|cx| {
            let _ = window.update(cx, |_, window, _| match window.render_to_image() {
                Ok(img) => img.save(&out).unwrap(),
                Err(e) => eprintln!("capture failed (debug builds only): {e}"),
            });
            cx.quit();
        });
    })
    .detach();
}
