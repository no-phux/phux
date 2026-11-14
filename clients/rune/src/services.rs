//! Finder's "New phux Terminal at Folder": the `NSServices` entry in the
//! bundle's Info.plist names `newTerminalAtFolder:userData:error:`, and the
//! provider here answers it with a new tab in that folder, in this window's
//! session.

use std::path::PathBuf;

/// The folders among the `file://` URLs a service was handed; a file stands
/// for the folder it is in.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn folders(urls: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    urls.into_iter()
        .filter_map(|url| url::Url::parse(&url).ok()?.to_file_path().ok())
        .filter_map(|path| {
            if path.is_dir() {
                Some(path)
            } else {
                path.parent().map(PathBuf::from)
            }
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub use mac::register;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mac {
    use std::cell::OnceCell;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send};
    use objc2_foundation::NSString;
    use rune::AppHandle;

    use crate::desk::Desk;

    thread_local! {
        /// The window the service opens in, and the provider AppKit holds
        /// weakly; set once, on the main thread.
        static PROVIDER: OnceCell<(AppHandle<Desk>, Retained<Provider>)> = const { OnceCell::new() };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "PhuxRuneServices"]
        struct Provider;

        impl Provider {
            #[unsafe(method(newTerminalAtFolder:userData:error:))]
            fn new_terminal_at_folder(
                &self,
                pasteboard: &AnyObject,
                _data: Option<&AnyObject>,
                _error: *mut *mut AnyObject,
            ) {
                let folders = super::folders(file_urls(pasteboard));
                PROVIDER.with(|provider| {
                    let Some((app, _)) = provider.get() else { return };
                    for folder in folders {
                        app.open();
                        app.update(move |desk, _| desk.terminal_at_folder(folder));
                    }
                });
            }
        }
    );

    /// The `public.file-url` strings on the service's pasteboard.
    fn file_urls(pasteboard: &AnyObject) -> Vec<String> {
        let kind = NSString::from_str("public.file-url");
        // Safety: AppKit hands the service an `NSPasteboard`, on the main
        // thread; these are its documented getters.
        unsafe {
            let items: Option<Retained<AnyObject>> = msg_send![pasteboard, pasteboardItems];
            let Some(items) = items else {
                return Vec::new();
            };
            let count: usize = msg_send![&items, count];
            (0..count)
                .filter_map(|index| {
                    let item: Retained<AnyObject> = msg_send![&items, objectAtIndex: index];
                    let url: Option<Retained<NSString>> = msg_send![&item, stringForType: &*kind];
                    url.map(|url| url.to_string())
                })
                .collect()
        }
    }

    /// Make this process the bundle's service provider. Call on the main
    /// thread once the app runs; headless runs have no services.
    pub fn register(app: AppHandle<Desk>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let provider: Retained<Provider> = unsafe { msg_send![Provider::alloc(mtm), init] };
        // Safety: `NSApp` lives for the process; main thread, checked above.
        unsafe {
            let ns_app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![&ns_app, setServicesProvider: &*provider];
        }
        PROVIDER.with(|cell| {
            let _ = cell.set((app, provider));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_url_is_that_folder_and_a_file_url_its_folder() {
        let dir = std::env::temp_dir().join(format!("phux-services-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let url = |p: &std::path::Path| url::Url::from_file_path(p).unwrap().to_string();
        let got = folders([url(&dir), url(&dir.join("a.txt")), "nonsense".to_owned()]);
        assert_eq!(got, [dir.clone(), dir.clone()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
