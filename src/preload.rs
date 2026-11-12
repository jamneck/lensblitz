//! Reading and parsing a document off the main thread.
//!
//! At launch the main thread spends ~100 ms creating the window and GL context. Files named
//! on the command line (or sent by Finder during launch) are read and parsed on a worker
//! thread during that time, so even a multi-megabyte document is ready when the window is.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::JoinHandle;

use crate::doc::{self, Doc};
use crate::log_err;
use crate::watch::{self, Signature};

pub struct Parsed {
    pub doc: Doc,
    /// File signature taken just before reading, for the live-reload watcher.
    pub seen: Signature,
}

type Pending = (PathBuf, JoinHandle<Result<Parsed, String>>);

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

/// Read and parse `path` (already absolute) right now, on this thread.
pub fn load(path: &Path) -> Result<Parsed, String> {
    let seen = watch::signature(path);
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            log_err!("{} is not valid UTF-8; showing it with replacement characters", path.display());
            String::from_utf8_lossy(err.as_bytes()).into_owned()
        }
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    Ok(Parsed { doc: doc::parse(text), seen })
}

/// Begin loading `path` in the background.
pub fn start(path: &Path) {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if pending.iter().any(|(p, _)| *p == path) {
        return;
    }
    let job = path.clone();
    match std::thread::Builder::new().name("lensblitz-load".to_owned()).spawn(move || load(&job)) {
        Ok(handle) => pending.push((path, handle)),
        Err(err) => log_err!("could not start a loader thread: {err}"),
    }
}

/// The result of an earlier [`start`] for `path`, waiting for it if it is still running.
pub fn take(path: &Path) -> Option<Result<Parsed, String>> {
    let handle = {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let index = pending.iter().position(|(p, _)| p == path)?;
        pending.swap_remove(index).1
    };
    Some(handle.join().unwrap_or_else(|_| Err("the loader thread panicked".to_owned())))
}
