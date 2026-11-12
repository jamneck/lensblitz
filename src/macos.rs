//! The little bit of AppKit that winit does not expose: receiving "open this file" events
//! from Finder / `open`, and the standard Open panel.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Height of a standard window title bar on this version of macOS, in points.
pub fn titlebar_height() -> f32 {
    static HEIGHT: OnceLock<f32> = OnceLock::new();
    *HEIGHT.get_or_init(|| imp::titlebar_height().filter(|h| (16.0..=64.0).contains(h)).unwrap_or(28.0))
}

/// Files the system asked us to open since the last call.
pub fn take_opened() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Let the open handler wake the UI when files arrive while the app is running.
pub fn set_context(ctx: &egui::Context) {
    let _ = CONTEXT.set(ctx.clone());
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_char;
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{MainThreadMarker, class, ffi, msg_send, sel};
    use objc2_foundation::{NSArray, NSString};

    use super::{CONTEXT, OPENED};
    use crate::log_err;

    /// `-[NSApplicationDelegate application:openURLs:]`
    unsafe extern "C-unwind" fn open_urls(
        _this: *mut AnyObject,
        _cmd: Sel,
        _app: *mut AnyObject,
        urls: *mut AnyObject,
    ) {
        let Some(urls) = (unsafe { urls.as_ref() }) else {
            return;
        };
        let mut paths = Vec::new();
        // SAFETY: AppKit passes an NSArray of NSURL.
        unsafe {
            let count: usize = msg_send![urls, count];
            for i in 0..count {
                let url: Option<Retained<AnyObject>> = msg_send![urls, objectAtIndex: i];
                let Some(url) = url else { continue };
                let path: Option<Retained<NSString>> = msg_send![&*url, path];
                if let Some(path) = path {
                    paths.push(PathBuf::from(path.to_string()));
                }
            }
        }
        if paths.is_empty() {
            return;
        }
        if CONTEXT.get().is_none() {
            // Still launching (a Finder double-click): parse while the window is being made.
            for path in &paths {
                crate::preload::start(path);
            }
        }
        OPENED.lock().unwrap_or_else(|e| e.into_inner()).extend(paths);
        if let Some(ctx) = CONTEXT.get() {
            ctx.request_repaint_of(eframe::egui::ViewportId::ROOT);
        }
    }

    /// `-[NSApplicationDelegate applicationSupportsSecureRestorableState:]`
    unsafe extern "C-unwind" fn secure_state(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject) -> Bool {
        Bool::YES
    }

    /// Teach winit's application delegate to accept files from Finder, the Dock and `open`.
    ///
    /// winit owns the delegate and does not handle these events, so we add the methods to
    /// its class. Must be called after the event loop is created and before it runs, so the
    /// "open" event of a double-click launch (delivered before `applicationDidFinishLaunching`)
    /// is not missed.
    pub fn install_open_handler() {
        if MainThreadMarker::new().is_none() {
            log_err!("open handler must be installed on the main thread");
            return;
        }
        // SAFETY: standard Objective-C runtime calls on the main thread. The added methods
        // match the selectors' signatures.
        unsafe {
            let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
            let delegate: Option<Retained<AnyObject>> = msg_send![&*app, delegate];
            let Some(delegate) = delegate else {
                log_err!("no application delegate; files opened from Finder will be ignored");
                return;
            };
            let cls: *const AnyClass = msg_send![&*delegate, class];
            let cls = cls as *mut AnyClass;

            let open: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) = open_urls;
            let open: Imp = std::mem::transmute(open);
            let types: *const c_char = c"v@:@@".as_ptr();
            if !ffi::class_addMethod(cls, sel!(application:openURLs:), open, types).as_bool() {
                log_err!("could not install the open-file handler");
            }

            let secure: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool = secure_state;
            let secure: Imp = std::mem::transmute(secure);
            let types: *const c_char = if cfg!(target_arch = "aarch64") { c"B@:@".as_ptr() } else { c"c@:@".as_ptr() };
            ffi::class_addMethod(cls, sel!(applicationSupportsSecureRestorableState:), secure, types);

            // Re-assign the delegate so AppKit notices the new optional methods.
            let nil: *const AnyObject = std::ptr::null();
            let _: () = msg_send![&*app, setDelegate: nil];
            let _: () = msg_send![&*app, setDelegate: &*delegate];
        }
    }

    /// The title bar height AppKit would add to a titled window (28 pt for years, taller on
    /// recent systems). Our content runs under the title bar, so we need the real number.
    pub fn titlebar_height() -> Option<f32> {
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        MainThreadMarker::new()?;
        const TITLED: usize = 1;
        let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 200.0));
        // SAFETY: `+[NSWindow frameRectForContentRect:styleMask:]` takes and returns an NSRect.
        let frame: NSRect = unsafe { msg_send![class!(NSWindow), frameRectForContentRect: content, styleMask: TITLED] };
        Some((frame.size.height - content.size.height) as f32)
    }

    /// Run as a bare binary (`lensblitz file.md`, not from LensBlitz.app) the Dock would show
    /// a generic executable icon; give it ours.
    pub fn set_dock_icon_if_unbundled() {
        static ICON: &[u8] = include_bytes!("../assets/icon/icon-256.png");
        if MainThreadMarker::new().is_none() {
            return;
        }
        // SAFETY: documented AppKit/Foundation selectors, called on the main thread. `ICON`
        // is static, and NSData copies it anyway.
        unsafe {
            let bundle: Retained<AnyObject> = msg_send![class!(NSBundle), mainBundle];
            let identifier: Option<Retained<NSString>> = msg_send![&*bundle, bundleIdentifier];
            if identifier.is_some() {
                return;
            }
            let data: Retained<AnyObject> =
                msg_send![class!(NSData), dataWithBytes: ICON.as_ptr().cast::<std::ffi::c_void>(), length: ICON.len()];
            let alloc: objc2::rc::Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
            let image: Option<Retained<AnyObject>> = msg_send![alloc, initWithData: &*data];
            if let Some(image) = image {
                let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
                let _: () = msg_send![&*app, setApplicationIconImage: &*image];
            }
        }
    }

    /// Show the standard Open panel and return the chosen files.
    pub fn open_panel() -> Vec<PathBuf> {
        if MainThreadMarker::new().is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        // SAFETY: NSOpenPanel is used on the main thread with documented selectors.
        unsafe {
            let panel: Retained<AnyObject> = msg_send![class!(NSOpenPanel), openPanel];
            let _: () = msg_send![&*panel, setAllowsMultipleSelection: true];
            let _: () = msg_send![&*panel, setCanChooseDirectories: false];
            let exts: Vec<Retained<NSString>> =
                crate::MARKDOWN_EXTENSIONS.iter().map(|e| NSString::from_str(e)).collect();
            let exts = NSArray::from_retained_slice(&exts);
            let responds: bool = msg_send![&*panel, respondsToSelector: sel!(setAllowedFileTypes:)];
            if responds {
                let _: () = msg_send![&*panel, setAllowedFileTypes: &*exts];
            }
            let response: isize = msg_send![&*panel, runModal];
            if response == 1 {
                let urls: Retained<AnyObject> = msg_send![&*panel, URLs];
                let count: usize = msg_send![&*urls, count];
                for i in 0..count {
                    let url: Option<Retained<AnyObject>> = msg_send![&*urls, objectAtIndex: i];
                    let Some(url) = url else { continue };
                    let path: Option<Retained<NSString>> = msg_send![&*url, path];
                    if let Some(path) = path {
                        out.push(PathBuf::from(path.to_string()));
                    }
                }
            }
        }
        out
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::PathBuf;

    pub fn install_open_handler() {}

    pub fn set_dock_icon_if_unbundled() {}

    pub fn titlebar_height() -> Option<f32> {
        Some(0.0).filter(|_| false)
    }

    pub fn open_panel() -> Vec<PathBuf> {
        Vec::new()
    }
}

pub use imp::{install_open_handler, open_panel, set_dock_icon_if_unbundled};

/// Open a URL or file with the system default handler.
pub fn open_external(target: &str) {
    spawn_open(&[target]);
}

/// Reveal a file in Finder.
pub fn reveal(path: &std::path::Path) {
    spawn_open(&["-R", &path.to_string_lossy()]);
}

fn spawn_open(args: &[&str]) {
    let program = if cfg!(target_os = "macos") { "/usr/bin/open" } else { "xdg-open" };
    let args: Vec<&str> =
        if cfg!(target_os = "macos") { args.to_vec() } else { args.iter().copied().filter(|a| *a != "-R").collect() };
    match std::process::Command::new(program).args(&args).spawn() {
        Ok(mut child) => {
            // Reap the short-lived helper so it does not linger as a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => crate::log_err!("could not run {program}: {err}"),
    }
}
