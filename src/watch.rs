//! Live reload. One background thread watches every open document.
//!
//! On macOS/BSD a kqueue wakes the thread the instant a file is written, replaced or
//! removed. The decision to reload is always made by comparing `stat` results, so editors
//! that save by renaming a temp file over the original are handled, and a slow periodic
//! check catches anything the kernel did not report (network volumes, for example).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, SystemTime};

use eframe::egui;

use crate::log_err;

/// What we compare to decide a file changed: modification time, length and inode.
pub type Signature = Option<(SystemTime, u64, u64)>;

pub fn signature(path: &std::path::Path) -> Signature {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    Some((meta.modified().ok()?, meta.len(), inode))
}

enum Cmd {
    Watch(Watch),
    Unwatch(u64),
}

struct Watch {
    id: u64,
    path: PathBuf,
    /// Signature of the content the window currently shows.
    seen: Signature,
    changed: Arc<AtomicBool>,
    viewport: egui::ViewportId,
    #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    fd: Option<i32>,
}

pub struct Watcher {
    tx: Sender<Cmd>,
    waker: sys::Waker,
}

impl Watcher {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let (waker, waiter) = sys::pair();
        let spawned =
            std::thread::Builder::new().name("lensblitz-watch".to_owned()).spawn(move || run(&rx, waiter, &ctx));
        if let Err(err) = spawned {
            log_err!("could not start the file watcher; live reload is off: {err}");
        }
        Self { tx, waker }
    }

    /// Watch `path` for window `id`. `seen` is the signature of the content already loaded;
    /// `changed` is set (and the window repainted) whenever the file differs from it.
    pub fn watch(&self, id: u64, path: PathBuf, seen: Signature, changed: Arc<AtomicBool>, viewport: egui::ViewportId) {
        let watch = Watch {
            id,
            path,
            seen,
            changed,
            viewport,
            #[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
            fd: None,
        };
        if self.tx.send(Cmd::Watch(watch)).is_ok() {
            self.waker.wake();
        }
    }

    pub fn unwatch(&self, id: u64) {
        if self.tx.send(Cmd::Unwatch(id)).is_ok() {
            self.waker.wake();
        }
    }
}

fn run(rx: &Receiver<Cmd>, mut waiter: sys::Waiter, ctx: &egui::Context) {
    let mut watches: Vec<Watch> = Vec::new();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Watch(w)) => {
                    watches.iter_mut().filter(|o| o.id == w.id).for_each(|o| waiter.release(o));
                    watches.retain(|o| o.id != w.id);
                    watches.push(w);
                }
                Ok(Cmd::Unwatch(id)) => {
                    watches.iter_mut().filter(|o| o.id == id).for_each(|o| waiter.release(o));
                    watches.retain(|o| o.id != id);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let mut all_armed = true;
        for w in &mut watches {
            all_armed &= waiter.arm(w);
        }
        for w in &mut watches {
            let now = signature(&w.path);
            if now != w.seen {
                w.seen = now;
                // A file that vanished keeps its last content on screen; only report content.
                if now.is_some() {
                    w.changed.store(true, Ordering::Release);
                    ctx.request_repaint_of(w.viewport);
                }
            }
        }

        // With every file armed we only wake on events (plus a slow safety check);
        // while a file is missing (mid-save) we look again quickly.
        let timeout = if watches.is_empty() {
            Duration::from_secs(3600)
        } else if all_armed {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(60)
        };
        if waiter.wait(timeout, &mut watches) {
            // Let a burst of writes settle so we reload once, with complete content.
            std::thread::sleep(Duration::from_millis(12));
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
mod sys {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    use super::Watch;
    use crate::log_err;

    const WAKE_IDENT: usize = 1;

    pub struct Waker {
        kq: i32,
    }

    pub struct Waiter {
        kq: i32,
    }

    pub fn pair() -> (Waker, Waiter) {
        // SAFETY: plain syscalls; a failed kqueue (-1) makes every later call fail harmlessly
        // and the watcher degrades to its periodic check.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            log_err!("kqueue failed: {}; live reload will poll", std::io::Error::last_os_error());
        } else {
            let ev = kevent(WAKE_IDENT, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0);
            // SAFETY: `ev` is a valid kevent and no events are read back.
            unsafe { libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        }
        (Waker { kq }, Waiter { kq })
    }

    fn kevent(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
        libc::kevent { ident, filter, flags, fflags, data: 0, udata: std::ptr::null_mut() }
    }

    impl Waker {
        pub fn wake(&self) {
            let ev = kevent(WAKE_IDENT, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
            // SAFETY: `ev` is a valid kevent and no events are read back.
            unsafe { libc::kevent(self.kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        }
    }

    impl Waiter {
        /// Make sure `w` has an open descriptor registered with the kqueue.
        /// Returns false while the file cannot be opened (it is missing).
        pub fn arm(&mut self, w: &mut Watch) -> bool {
            if w.fd.is_some() {
                return true;
            }
            let Ok(path) = CString::new(w.path.as_os_str().as_bytes()) else {
                return true;
            };
            // SAFETY: `path` is a valid NUL-terminated string.
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return false;
            }
            let ev = kevent(
                fd as usize,
                libc::EVFILT_VNODE,
                libc::EV_ADD | libc::EV_CLEAR,
                libc::NOTE_WRITE
                    | libc::NOTE_EXTEND
                    | libc::NOTE_ATTRIB
                    | libc::NOTE_DELETE
                    | libc::NOTE_RENAME
                    | libc::NOTE_REVOKE,
            );
            // SAFETY: `ev` is a valid kevent for an open descriptor; no events are read back.
            let rc = unsafe { libc::kevent(self.kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
            if rc < 0 {
                // SAFETY: `fd` was opened above and is not used again.
                unsafe { libc::close(fd) };
                return true; // Fall back to the periodic check for this file.
            }
            w.fd = Some(fd);
            true
        }

        pub fn release(&mut self, w: &mut Watch) {
            if let Some(fd) = w.fd.take() {
                // SAFETY: `fd` is owned by this watch; closing it also removes its kevents.
                unsafe { libc::close(fd) };
            }
        }

        /// Block until a watched file changes, the waker fires, or `timeout` passes.
        /// Returns true if a file event arrived.
        pub fn wait(&mut self, timeout: Duration, watches: &mut [Watch]) -> bool {
            if self.kq < 0 {
                std::thread::sleep(timeout.min(Duration::from_millis(400)));
                return false;
            }
            let ts = libc::timespec { tv_sec: timeout.as_secs() as _, tv_nsec: timeout.subsec_nanos() as _ };
            let mut events = [kevent(0, 0, 0, 0); 16];
            // SAFETY: `events` has room for 16 entries and `ts` outlives the call.
            let n = unsafe { libc::kevent(self.kq, std::ptr::null(), 0, events.as_mut_ptr(), 16, &ts) };
            if n < 0 {
                // Interrupted or failed: never spin, fall back to the periodic check.
                std::thread::sleep(timeout.min(Duration::from_millis(200)));
                return false;
            }
            let mut file_event = false;
            for ev in events.iter().take(n.max(0) as usize) {
                if ev.filter != libc::EVFILT_VNODE {
                    continue;
                }
                file_event = true;
                // The path now points at a different file (or nothing): re-open next round.
                if ev.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE) != 0 {
                    for w in watches.iter_mut().filter(|w| w.fd == Some(ev.ident as i32)) {
                        self.release(w);
                    }
                }
            }
            file_event
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd")))]
mod sys {
    use std::time::Duration;

    use super::Watch;

    pub struct Waker;
    pub struct Waiter;

    pub fn pair() -> (Waker, Waiter) {
        (Waker, Waiter)
    }

    impl Waker {
        pub fn wake(&self) {}
    }

    impl Waiter {
        pub fn arm(&mut self, _w: &mut Watch) -> bool {
            true
        }

        pub fn release(&mut self, _w: &mut Watch) {}

        pub fn wait(&mut self, timeout: Duration, _watches: &mut [Watch]) -> bool {
            std::thread::sleep(timeout.min(Duration::from_millis(250)));
            false
        }
    }
}
