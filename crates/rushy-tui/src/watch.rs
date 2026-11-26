//! Config hot-reload: watches the config directory for changes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{Event, EventKind, PollWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::{Receiver, Sender, channel};

use crate::Result;

const DEBOUNCE: Duration = Duration::from_millis(200);
const MAX_DEBOUNCE: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Whether a change to `path` can affect the configuration: TOML, bundle
/// scripts, and README files. Databases, sockets, extensionless temp/lock
/// files, and hidden files (such as Finder's `.DS_Store`) are ignored.
#[must_use]
pub fn is_config_path(path: &Path) -> bool {
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
    {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        None => false,
        Some(e) => matches!(e.to_ascii_lowercase().as_str(), "toml" | "rushy" | "md"),
    }
}

fn handler(tx: Sender<()>, root: PathBuf) -> impl FnMut(notify::Result<Event>) + Send + 'static {
    move |res| {
        if let Ok(ev) = res {
            let kind_ok = matches!(
                ev.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if kind_ok && ev.paths.iter().any(|p| *p != root && is_config_path(p)) {
                // One pending signal is enough: `changed` drains bursts, and a
                // full queue already means "something changed".
                let _ = tx.try_send(());
            }
        }
    }
}

/// Watches a directory recursively and reports batches of changes.
///
/// It uses `FSEvents` and falls back to polling every 500 ms when that cannot
/// start, for example when the directory is on a network filesystem.
pub struct ConfigWatcher {
    // Held to keep the watch alive.
    _watcher: Box<dyn Watcher + Send>,
    rx: Receiver<()>,
    polling: bool,
}

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigWatcher")
            .field("polling", &self.polling)
            .finish_non_exhaustive()
    }
}

impl ConfigWatcher {
    /// Starts watching `dir`, creating it if needed.
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be created or neither watcher can start.
    pub fn new(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (tx, rx) = channel(1);
        let native = notify::recommended_watcher(handler(tx.clone(), dir.to_path_buf())).and_then(
            |mut w| {
                w.watch(dir, RecursiveMode::Recursive)?;
                Ok(w)
            },
        );
        let (watcher, polling): (Box<dyn Watcher + Send>, bool) = if let Ok(w) = native {
            (Box::new(w), false)
        } else {
            let mut w = PollWatcher::new(
                handler(tx, dir.to_path_buf()),
                notify::Config::default().with_poll_interval(POLL_INTERVAL),
            )?;
            w.watch(dir, RecursiveMode::Recursive)?;
            (Box::new(w), true)
        };
        Ok(Self {
            _watcher: watcher,
            rx,
            polling,
        })
    }

    /// Whether the polling fallback is in use.
    #[must_use]
    pub fn is_polling(&self) -> bool {
        self.polling
    }

    /// Waits for a change, then until the directory is quiet for 200 ms (capped
    /// at 1 s total debounce wait). Returns `false` when the watcher has stopped.
    pub async fn changed(&mut self) -> bool {
        if self.rx.recv().await.is_none() {
            return false;
        }
        let deadline = tokio::time::Instant::now() + MAX_DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let wait = DEBOUNCE.min(remaining);
            match tokio::time::timeout(wait, self.rx.recv()).await {
                Ok(Some(())) => {}
                Ok(None) | Err(_) => break,
            }
        }
        true
    }
}
