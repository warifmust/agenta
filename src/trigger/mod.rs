pub mod file_watcher;
pub mod http_trigger;
pub mod command_trigger;
pub mod scheduler;

pub use file_watcher::FileWatcherTrigger;
pub use http_trigger::HttpTrigger;
pub use command_trigger::CommandTrigger;
pub use scheduler::Scheduler;

use crate::core::TriggerEvent;
use tokio::sync::mpsc;

/// Trait for triggers
#[async_trait::async_trait]
pub trait Trigger: Send + Sync {
    /// Start the trigger
    async fn start(&self, event_sender: mpsc::Sender<TriggerEvent>) -> crate::core::Result<()>;

    /// Stop the trigger
    async fn stop(&self) -> crate::core::Result<()>;
}

/// Trigger manager that coordinates all triggers
pub struct TriggerManager {
    event_sender: mpsc::Sender<TriggerEvent>,
}

impl TriggerManager {
    pub fn new(event_sender: mpsc::Sender<TriggerEvent>) -> Self {
        Self { event_sender }
    }
}
