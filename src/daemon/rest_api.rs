use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use agenta::core::{
    Agent, AgentConfig, AgentEnv, AgentStatus, AppConfig, DeepAgentConfig, ExecutionMode,
    ExecutionResult, ExecutionStatus, ToolCall, ToolDefinition, TriggerType,
};
use super::state::DaemonState;

#[derive(Clone)]
struct ApiState {
    daemon: Arc<DaemonState>,
}

#[derive(Clone)]
struct AuthState {
    token: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct MessageResponse {
    message: String,
}

#[derive(Deserialize, ToSchema)]
struct CreateAgentBody {
    agent: Agent,
}

#[derive(Deserialize, ToSchema)]
struct UpdateAgentBody {
    agent: Agent,
}

#[derive(Deserialize, ToSchema)]
struct RunBody {
    input: Option<String>,
}

#[derive(Deserialize, ToSchema)]
struct ListExecutionsQuery {
    limit: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
struct LogsQuery {
    execution_id: Option<String>,
    lines: Option<usize>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        health,
        list_agents,
        get_agent,
        create_agent,
        update_agent,
        delete_agent,
        run_agent,
        get_execution,
        list_executions,
        get_logs
    ),
    components(
        schemas(
            Agent,
            AgentConfig,
            AgentEnv,
            AgentStatus,
            DeepAgentConfig,
            ExecutionMode,
            ExecutionResult,
            ExecutionStatus,
            ToolCall,
            ToolDefinition,
            TriggerType,
            MessageResponse,
            CreateAgentBody,
            UpdateAgentBody,
            RunBody,
            ListExecutionsQuery,
            LogsQuery
        )
    ),
    tags(
        (name = "agenta", description = "Agenta REST API")
    )
)]
struct ApiDoc;

pub async fn start_rest_api(daemon: Arc<DaemonState>, config: &AppConfig) -> anyhow::Result<()> {
    let api_state = ApiState { daemon };
    let auth_state = AuthState {
        token: config.api_token.clone(),
    };

    let protected_api = Router::new()
        .route("/health", get(health))
        .route("/agents", get(list_agents).post(create_agent))
        .route(
            "/agents/:id",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/agents/:id/run", post(run_agent))
        .route("/agents/:id/executions", get(list_executions))
        .route("/agents/:id/logs", get(get_logs))
        .route("/executions/:id", get(get_execution))
        .with_state(api_state.clone())
        .route_layer(middleware::from_fn_with_state(auth_state, auth_middleware));

    let app = Router::new()
        .nest("/api", protected_api)
        .merge(SwaggerUi::new("/swagger-ui").url("/api-doc/openapi.json", ApiDoc::openapi()));

    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", config.api_port)
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid API bind address: {}", e))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("REST API listening on http://127.0.0.1:{}", config.api_port);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("REST API failed: {}", e);
        }
    });

    Ok(())
}

async fn auth_middleware(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(expected) = auth.token.as_deref().filter(|s| !s.is_empty()) else {
        return next.run(request).await;
    };

    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.to_string());

    if api_key.as_deref() == Some(expected) || bearer.as_deref() == Some(expected) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

#[utoipa::path(get, path = "/api/health", responses((status = 200, body = MessageResponse)))]
async fn health() -> Json<MessageResponse> {
    Json(MessageResponse {
        message: "ok".to_string(),
    })
}

#[utoipa::path(get, path = "/api/agents", responses((status = 200, body = [Agent])))]
async fn list_agents(
    State(state): State<ApiState>,
) -> Result<Json<Vec<Agent>>, (StatusCode, String)> {
    state
        .daemon
        .list_agents()
        .await
        .map(Json)
        .map_err(internal_error)
}

#[utoipa::path(get, path = "/api/agents/{id}", responses((status = 200, body = Agent), (status = 404, body = MessageResponse)))]
async fn get_agent(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Agent>, (StatusCode, String)> {
    match state.daemon.get_agent(&id).await.map_err(internal_error)? {
        Some(agent) => Ok(Json(agent)),
        None => Err((StatusCode::NOT_FOUND, "Agent not found".to_string())),
    }
}

#[utoipa::path(post, path = "/api/agents", request_body = CreateAgentBody, responses((status = 200, body = MessageResponse)))]
async fn create_agent(
    State(state): State<ApiState>,
    Json(body): Json<CreateAgentBody>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let id = state
        .daemon
        .create_agent(body.agent)
        .await
        .map_err(internal_error)?;
    Ok(Json(MessageResponse {
        message: format!("Agent created: {}", id),
    }))
}

#[utoipa::path(put, path = "/api/agents/{id}", request_body = UpdateAgentBody, responses((status = 200, body = MessageResponse)))]
async fn update_agent(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateAgentBody>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    state
        .daemon
        .update_agent(id, body.agent)
        .await
        .map_err(internal_error)?;
    Ok(Json(MessageResponse {
        message: "Agent updated".to_string(),
    }))
}

#[utoipa::path(delete, path = "/api/agents/{id}", responses((status = 200, body = MessageResponse), (status = 404, body = MessageResponse)))]
async fn delete_agent(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    match state.daemon.delete_agent(&id).await.map_err(internal_error)? {
        true => Ok(Json(MessageResponse {
            message: "Agent deleted".to_string(),
        })),
        false => Err((StatusCode::NOT_FOUND, "Agent not found".to_string())),
    }
}

#[utoipa::path(post, path = "/api/agents/{id}/run", request_body = RunBody, responses((status = 200, body = MessageResponse)))]
async fn run_agent(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<RunBody>,
) -> Result<Json<MessageResponse>, (StatusCode, String)> {
    let execution_id = state
        .daemon
        .run_agent(&id, body.input)
        .await
        .map_err(internal_error)?;
    Ok(Json(MessageResponse {
        message: execution_id,
    }))
}

#[utoipa::path(get, path = "/api/executions/{id}", responses((status = 200, body = ExecutionResult), (status = 404, body = MessageResponse)))]
async fn get_execution(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<ExecutionResult>, (StatusCode, String)> {
    match state.daemon.get_execution(&id).await.map_err(internal_error)? {
        Some(execution) => Ok(Json(execution)),
        None => Err((StatusCode::NOT_FOUND, "Execution not found".to_string())),
    }
}

#[utoipa::path(get, path = "/api/agents/{id}/executions", responses((status = 200, body = [ExecutionResult])))]
async fn list_executions(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<ListExecutionsQuery>,
) -> Result<Json<Vec<ExecutionResult>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(20);
    let executions = state
        .daemon
        .list_executions_for_agent(&id, limit)
        .await
        .map_err(internal_error)?;
    Ok(Json(executions))
}

#[utoipa::path(get, path = "/api/agents/{id}/logs", responses((status = 200, body = [String])))]
async fn get_logs(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let lines = query.lines.unwrap_or(50);
    state
        .daemon
        .get_logs(&id, query.execution_id.as_deref(), lines)
        .await
        .map(Json)
        .map_err(internal_error)
}

fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
