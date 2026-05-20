use chrono::Utc;
use chrono_tz::Tz;
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
    timezone: Tz,
}

/// Resolve timezone: use config value if set, else fall back to system local offset.
pub fn resolve_timezone(config_tz: Option<&str>) -> Tz {
    if let Some(tz_str) = config_tz {
        match tz_str.parse::<Tz>() {
            Ok(tz) => {
                info!("Scheduler timezone: {} (from config)", tz_str);
                return tz;
            }
            Err(_) => {
                tracing::warn!("Invalid timezone '{}' in config — falling back to system timezone", tz_str);
            }
        }
    }

    // Fall back to system local timezone offset
    let offset_secs = chrono::Local::now().offset().local_minus_utc();
    let offset_hours = offset_secs / 3600;
    let tz_name = format!("Etc/GMT{:+}", -offset_hours); // Etc/GMT uses inverted sign
    match tz_name.parse::<Tz>() {
        Ok(tz) => {
            info!("Scheduler timezone: {} (from system, UTC{:+})", tz_name, offset_hours);
            tz
        }
        Err(_) => {
            info!("Scheduler timezone: UTC (system timezone detection failed)");
            Tz::UTC
        }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::with_timezone(Tz::UTC)
    }

    pub fn with_timezone(timezone: Tz) -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(RwLock::new(false)),
            timezone,
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
        let timezone = self.timezone;

        tokio::spawn(async move {
            while *running.read().await {
                let now_utc = Utc::now();
                let now_local = now_utc.with_timezone(&timezone);

                let jobs_to_run: Vec<String> = {
                    let jobs_read = jobs.read().await;
                    jobs_read
                        .values()
                        .filter_map(|job| {
                            // Evaluate cron expression in local timezone so "9am" means 9am locally
                            let reference = if let Some(last) = job.last_run {
                                last.with_timezone(&timezone)
                            } else {
                                now_local
                            };

                            let next_run = job.schedule.after(&reference).next();

                            next_run.and_then(|next| {
                                if next <= now_local {
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
                        job.last_run = Some(now_utc);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_timezone_parses_valid_iana_name() {
        let tz = resolve_timezone(Some("Asia/Kuala_Lumpur"));
        assert_eq!(tz, chrono_tz::Asia::Kuala_Lumpur);
    }

    #[test]
    fn resolve_timezone_parses_utc() {
        let tz = resolve_timezone(Some("UTC"));
        assert_eq!(tz, Tz::UTC);
    }

    #[test]
    fn resolve_timezone_falls_back_for_invalid_string() {
        // Invalid timezone name — should not panic, should return *some* Tz
        let tz = resolve_timezone(Some("Not/AReal_Zone"));
        // We can't assert a specific value since it falls back to system tz,
        // but it must be a valid Tz variant (i.e., the call must succeed).
        let _name = format!("{}", tz); // would panic if Tz were in a bad state
    }

    #[test]
    fn resolve_timezone_falls_back_when_none() {
        // None → system timezone (or UTC if detection fails) — must not panic
        let tz = resolve_timezone(None);
        let _name = format!("{}", tz);
    }

    #[test]
    fn scheduler_default_uses_utc() {
        let s = Scheduler::new();
        assert_eq!(s.timezone, Tz::UTC);
    }

    #[test]
    fn scheduler_with_timezone_stores_tz() {
        let tz = chrono_tz::Asia::Tokyo;
        let s = Scheduler::with_timezone(tz);
        assert_eq!(s.timezone, tz);
    }
}
