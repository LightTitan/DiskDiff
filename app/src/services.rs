//! Finder's "Temporary Scan with DiskDiff" service.
//!
//! The NSServices entry in Info.plist (see build.sh) only declares the menu
//! item; macOS delivers the selected folder by calling the method named by
//! NSMessage on the app's services provider. GPUI never registers one, so this
//! module does and forwards the folder into a temporary scan.

use crate::state::AppState;
use gpui_kit::*;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub fn init(state: Entity<AppState>, cx: &mut App) {
    let (tx, rx) = futures::channel::mpsc::unbounded::<PathBuf>();
    if mac::register(tx).is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        let mut rx = rx;
        while let Some(path) = rx.next().await {
            let _ = cx.update(|cx| {
                cx.activate(true);
                state.update(cx, |s, cx| s.start_scan(path, true, cx));
            });
        }
    })
    .detach();
}

#[cfg(not(target_os = "macos"))]
pub fn init(_state: Entity<AppState>, _cx: &mut App) {}

#[cfg(target_os = "macos")]
mod mac {
    use futures::channel::mpsc::UnboundedSender;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSPasteboard, NSPasteboardTypeFileURL, NSUpdateDynamicServices};
    use objc2_foundation::{NSString, NSURL};
    use std::path::PathBuf;
    use std::sync::OnceLock;

    static SENDER: OnceLock<UnboundedSender<PathBuf>> = OnceLock::new();

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "DiskDiffServicesProvider"]
        struct ServicesProvider;

        impl ServicesProvider {
            // The selector must match NSMessage ("temporaryScan") in Info.plist.
            #[unsafe(method(temporaryScan:userData:error:))]
            fn temporary_scan(&self, pboard: &NSPasteboard, _user_data: Option<&NSString>, _error: *mut *mut NSString) {
                // A scan has one root; with a Finder multi-selection take the first folder.
                let Some(path) = first_file_path(pboard) else { return };
                if let Some(tx) = SENDER.get() {
                    let _ = tx.unbounded_send(path);
                }
            }
        }
    );

    fn first_file_path(pboard: &NSPasteboard) -> Option<PathBuf> {
        let items = pboard.pasteboardItems()?;
        items.iter().find_map(|item| {
            let url = item.stringForType(unsafe { NSPasteboardTypeFileURL })?;
            let path = NSURL::URLWithString(&url)?.path()?;
            Some(PathBuf::from(path.to_string()))
        })
    }

    pub fn register(tx: UnboundedSender<PathBuf>) -> Result<(), ()> {
        let mtm = MainThreadMarker::new().ok_or(())?;
        SENDER.set(tx).map_err(|_| ())?;
        let provider: Retained<ServicesProvider> = unsafe { msg_send![ServicesProvider::alloc(mtm), init] };
        let app = NSApplication::sharedApplication(mtm);
        unsafe { app.setServicesProvider(Some(&provider)) };
        // NSApp doesn't retain its services provider; it lives for the whole run.
        std::mem::forget(provider);
        // Pick up the Info.plist entry without a logout after an install.
        NSUpdateDynamicServices();
        Ok(())
    }
}
