use chrono::Utc;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::time::sleep;
use tracing::{error, info};

use crate::core::{Agent, AgentaError, Result, TriggerEvent};

pub struct ScheduledJob {
    pub agent_id: String,
    pub schedule: Schedule,
    pub last_run: Option<chrono::DateTime<Utc>>,
}

pub struct Scheduler {
    jobs: Arc<RwLock<HashMap<String, ScheduledJob>>>,
    running: Arc<RwLock<bool>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn add_job(&self, agent: &Agent) -> Result<()> {
        let cron_expr = agent
            .schedule
            .as_ref()
            .ok_or_else(|| AgentaError::InvalidCron("No schedule specified".to_string()))?;

        let schedule = Schedule::from_str(cron_expr)
            .map_err(|e| AgentaError::InvalidCron(format!("Invalid cron expression: {}", e)))?;

        let job = ScheduledJob {
            agent_id: agent.id.clone(),
            schedule,
            last_run: agent.last_run.or_else(|| Some(Utc::now())),
        };

        self.jobs.write().await.insert(agent.id.clone(), job);
        info!("Added scheduled job for agent: {}", agent.name);

        Ok(())
    }

    pub async fn remove_job(&self, agent_id: &str) -> bool {
        let removed = self.jobs.write().await.remove(agent_id).is_some();
        if removed {
            info!("Removed scheduled job for agent: {}", agent_id);
        }
        removed
    }

    pub async fn start(&self, event_sender: mpsc::Sender<TriggerEvent>) {
        *self.running.write().await = true;
        let running = self.running.clone();
        let jobs = self.jobs.clone();

        tokio::spawn(async move {
            while *running.read().await {
                let now = Utc::now();
                let jobs_to_run: Vec<String> = {
                    let jobs_read = jobs.read().await;
                    jobs_read
                        .values()
                        .filter_map(|job| {
                            let next_run = if let Some(last) = job.last_run {
                                job.schedule.after(&last).next()
                            } else {
                                job.schedule.after(&now).next()
                            };

                            next_run.and_then(|next| {
                                if next <= now {
                                    Some(job.agent_id.clone())
                                } else {
                                    None
                                }
                            })
                        })
                        .collect()
                };

                for agent_id in jobs_to_run {
                    let cron_expr = if let Some(job) = jobs.write().await.get_mut(&agent_id) {
                        job.last_run = Some(now);
                        job.schedule.to_string()
                    } else {
                        "unknown".to_string()
                    };

                    let event = TriggerEvent::Scheduled {
                        agent_id: agent_id.clone(),
                        cron: cron_expr,
                    };

                    if let Err(e) = event_sender.send(event).await {
                        error!("Failed to send scheduled event: {}", e);
                    }
                }

                sleep(Duration::from_secs(30)).await;
            }
        });

        info!("Scheduler started");
    }

    pub async fn stop(&self) {
        *self.running.write().await = false;
        info!("Scheduler stopped");
    }

    pub async fn list_jobs(&self) -> Vec<ScheduledJob> {
        self.jobs.read().await.values().cloned().collect()
    }
}

impl Clone for ScheduledJob {
    fn clone(&self) -> Self {
        Self {
            agent_id: self.agent_id.clone(),
            schedule: Schedule::from_str(&self.schedule.to_string()).unwrap(),
            last_run: self.last_run,
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
