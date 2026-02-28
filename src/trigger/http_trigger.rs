use axum::{
    extract::Path as AxumPath,
    extract::State,
    http::StatusCode,
    response::Json,
    routing::any,
    Router,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

use crate::core::{AgentaError, Result, TriggerEvent};

#[derive(Clone)]
struct AppState {
    event_sender: mpsc::Sender<TriggerEvent>,
    webhooks: Arc<RwLock<HashMap<String, WebhookConfig>>>, // path -> config
}

#[derive(Clone)]
struct WebhookConfig {
    agent_id: String,
    method: String,
}

pub struct HttpTrigger {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    webhooks: Arc<RwLock<HashMap<String, WebhookConfig>>>,
}

impl HttpTrigger {
    pub fn new() -> Self {
        Self {
            shutdown_tx: None,
            webhooks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start(
        &mut self,
        port: u16,
        event_sender: mpsc::Sender<TriggerEvent>,
    ) -> Result<()> {
        let state = AppState {
            event_sender,
            webhooks: self.webhooks.clone(),
        };

        let app = Router::new()
            .route("/health", any(health_check))
            .route("/webhook/:id", any(webhook_handler))
            .with_state(state);

        let addr: SocketAddr = format!("127.0.0.1:{}", port)
            .parse()
            .map_err(|e| AgentaError::Trigger(format!("Invalid address: {}", e)))?;

        let (shutdown_tx, _shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| AgentaError::Trigger(format!("Failed to bind: {}", e)))?;

        info!("HTTP trigger server starting on {}", addr);

        tokio::spawn(async move {
            let server = axum::serve(listener, app);
            if let Err(e) = server.await {
                error!("HTTP server error: {}", e);
            }
        });

        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    pub async fn register_webhook(
        &self,
        path: String,
        method: String,
        agent_id: String,
    ) {
        self.webhooks
            .write()
            .await
            .insert(path, WebhookConfig { agent_id, method });
    }

    pub async fn unregister_webhook(&self, agent_id: &str) {
        let mut webhooks = self.webhooks.write().await;
        webhooks.retain(|_, config| config.agent_id != agent_id);
    }
}

impl Default for HttpTrigger {
    fn default() -> Self {
        Self::new()
    }
}

async fn health_check() -> (StatusCode, String) {
    (StatusCode::OK, "OK".to_string())
}

async fn webhook_handler(
    AxumPath(path): AxumPath<String>,
    State(state): State<AppState>,
    method: axum::http::Method,
    body: Option<Json<serde_json::Value>>,
) -> (StatusCode, String) {
    let body_str = body.map(|b| b.0.to_string());

    let config = {
        let webhooks = state.webhooks.read().await;
        webhooks.get(&path).cloned()
    };

    let Some(config) = config else {
        return (StatusCode::NOT_FOUND, "Unknown webhook".to_string());
    };

    if method.to_string().to_lowercase() != config.method.to_lowercase() {
        return (StatusCode::METHOD_NOT_ALLOWED, "Invalid method".to_string());
    };

    let event = TriggerEvent::HttpRequest {
        agent_id: config.agent_id,
        method: method.to_string(),
        path: path.clone(),
        body: body_str,
    };

    if let Err(e) = state.event_sender.send(event).await {
        error!("Failed to send trigger event: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal error".to_string(),
        );
    }

    (StatusCode::OK, "OK".to_string())
}
