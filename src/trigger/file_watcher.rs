use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::core::{AgentaError, Result, TriggerEvent};

pub struct FileWatcherTrigger {
    watcher: Option<RecommendedWatcher>,
}

impl FileWatcherTrigger {
    pub fn new() -> Self {
        Self { watcher: None }
    }

    pub async fn watch_path(
        &mut self,
        path: &Path,
        recursive: bool,
        event_sender: mpsc::Sender<TriggerEvent>,
    ) -> Result<()> {
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            match res {
                Ok(event) => {
                    for path in event.paths {
                        let path_str = path.to_string_lossy().to_string();
                        let event = match event.kind {
                            notify::EventKind::Create(_) => {
                                Some(TriggerEvent::FileCreated { path: path_str })
                            }
                            notify::EventKind::Modify(_) => {
                                Some(TriggerEvent::FileModified { path: path_str })
                            }
                            notify::EventKind::Remove(_) => {
                                Some(TriggerEvent::FileDeleted { path: path_str })
                            }
                            _ => None,
                        };

                        if let Some(evt) = event {
                            if let Err(e) = event_sender.try_send(evt) {
                                error!("Failed to send file event: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("File watcher error: {}", e);
                }
            }
        })
        .map_err(|e| AgentaError::Trigger(format!("Failed to create watcher: {}", e)))?;

        let mut watcher = watcher;
        watcher
            .watch(path, mode)
            .map_err(|e| AgentaError::Trigger(format!("Failed to watch path: {}", e)))?;

        self.watcher = Some(watcher);
        info!("Started file watcher for path: {:?}", path);

        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if let Some(mut watcher) = self.watcher.take() {
            watcher
                .unwatch(std::path::Path::new("/"))
                .map_err(|e| AgentaError::Trigger(format!("Failed to unwatch: {}", e)))?;
        }
        Ok(())
    }
}

impl Default for FileWatcherTrigger {
    fn default() -> Self {
        Self::new()
    }
}
