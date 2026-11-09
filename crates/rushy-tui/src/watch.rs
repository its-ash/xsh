//! Config hot-reload: watches the config directory for changes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{Event, EventKind, PollWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::Result;

const DEBOUNCE: Duration = Duration::from_millis(200);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Whether a change to `path` can affect the configuration: TOML, bundle
/// scripts, README files, and directories (bundle installs and removals).
/// Databases, sockets, and caches that happen to live nearby are ignored, so
/// the shell's own writes (such as the history) never look like an edit.
#[must_use]
pub fn is_config_path(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        None => true,
        Some(e) => matches!(e.to_ascii_lowercase().as_str(), "toml" | "rushy" | "md"),
    }
}

fn handler(
    tx: UnboundedSender<()>,
    root: PathBuf,
) -> impl FnMut(notify::Result<Event>) + Send + 'static {
    move |res| {
        if let Ok(ev) = res {
            let kind_ok = matches!(
                ev.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if kind_ok && ev.paths.iter().any(|p| *p != root && is_config_path(p)) {
                let _ = tx.send(());
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
    rx: UnboundedReceiver<()>,
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
        let (tx, rx) = unbounded_channel();
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

    /// Waits for a change, then until the directory is quiet for 200 ms.
    /// Returns `false` when the watcher has stopped.
    pub async fn changed(&mut self) -> bool {
        if self.rx.recv().await.is_none() {
            return false;
        }
        while let Ok(Some(())) = tokio::time::timeout(DEBOUNCE, self.rx.recv()).await {}
        true
    }
}
