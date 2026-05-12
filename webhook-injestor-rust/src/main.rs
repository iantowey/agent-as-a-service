use axum::{
    extract::{State, Json},
    http::HeaderMap,
    routing::{get, post},
    Router,
};
use google_cloud_pubsub::client::{Client as PubSubClient, ClientConfig as PubSubConfig};
use pulsar::{Authentication, Pulsar, TokioExecutor};
use pulsar::producer::Producer;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

// --- Enum ADT State Management ---

pub enum WebhookRequest {
    Unvalidated {
        source: String,
        payload: Value,
        headers: HashMap<String, String>,
    },
    Validated {
        routing_key: String,
        task_event: Value,
    },
}

impl WebhookRequest {
    pub fn new(source: &str, payload: Value, headers: HashMap<String, String>) -> Self {
        WebhookRequest::Unvalidated {
            source: source.to_string(),
            payload,
            headers,
        }
    }

    pub fn validate_bitbucket(self) -> Result<Self, String> {
        match self {
            WebhookRequest::Unvalidated { payload, headers, .. } => {
                let event_key = headers.get("x-event-key").map(|s| s.as_str()).unwrap_or("unknown");

                if event_key == "pullrequest:comment_created" {
                    let comment_text = payload.pointer("/comment/text").and_then(|v| v.as_str()).unwrap_or("");
                    let author_name = payload.pointer("/actor/name").and_then(|v| v.as_str()).unwrap_or("");

                    if author_name == "Opencode Agent" || comment_text.contains("<<OPENCODE>>") {
                        return Err("anti_loop".to_string());
                    }

                    let pr_id = payload.pointer("/pullrequest/id").map(|v| {
                        if v.is_number() { v.as_i64().unwrap().to_string() } else { v.as_str().unwrap_or("").to_string() }
                    }).unwrap_or_default();

                    let repo_slug = payload.pointer("/repository/slug").and_then(|v| v.as_str()).unwrap_or("");
                    let project_key = payload.pointer("/repository/project/key").and_then(|v| v.as_str()).unwrap_or("");
                    let branch_name = payload.pointer("/pullrequest/source/branch/name").and_then(|v| v.as_str()).unwrap_or("");

                    if pr_id.is_empty() || repo_slug.is_empty() || project_key.is_empty() {
                        return Err("missing_fields".to_string());
                    }

                    let task_event = serde_json::json!({
                        "source": "bitbucket",
                        "event_type": "pr_comment",
                        "project_key": project_key,
                        "repository_slug": repo_slug,
                        "pr_id": pr_id,
                        "branch_name": branch_name,
                        "comment_text": comment_text,
                        "author": author_name,
                    });

                    let routing_key = format!("{}-{}-pr-{}", project_key, repo_slug, pr_id);

                    Ok(WebhookRequest::Validated { routing_key, task_event })
                } else {
                    Err(format!("unhandled_event_type: {}", event_key))
                }
            }
            _ => Err("Invalid state transition: Request is already validated.".to_string()),
        }
    }

    pub fn validate_jira(self) -> Result<Self, String> {
        match self {
            WebhookRequest::Unvalidated { payload, .. } => {
                let webhook_event = payload.pointer("/webhookEvent").and_then(|v| v.as_str()).unwrap_or("unknown");

                if webhook_event == "jira:issue_created" || webhook_event == "jira:issue_updated" {
                    let issue_key = payload.pointer("/issue/key").and_then(|v| v.as_str()).unwrap_or("");
                    if issue_key.is_empty() {
                        return Err("no_issue_key".to_string());
                    }

                    let summary = payload.pointer("/issue/fields/summary").and_then(|v| v.as_str()).unwrap_or("");
                    let description = payload.pointer("/issue/fields/description").and_then(|v| v.as_str()).unwrap_or("");
                    let status = payload.pointer("/issue/fields/status/name").and_then(|v| v.as_str()).unwrap_or("");

                    let event_type = webhook_event.split(':').last().unwrap_or("");

                    let task_event = serde_json::json!({
                        "source": "jira",
                        "event_type": event_type,
                        "issue_key": issue_key,
                        "summary": summary,
                        "description": description,
                        "status": status,
                    });

                    let routing_key = format!("jira-{}", issue_key);

                    Ok(WebhookRequest::Validated { routing_key, task_event })
                } else {
                    Err(format!("unhandled_event_type: {}", webhook_event))
                }
            }
            _ => Err("Invalid state transition: Request is already validated.".to_string()),
        }
    }

    pub fn validate_pubsub_jira(self) -> Result<Self, String> {
        match self {
            WebhookRequest::Unvalidated { payload, .. } => {
                let issue = payload.get("issue").and_then(|v| v.as_object());
                if issue.is_none() {
                    return Err("missing_issue_object".to_string());
                }
                let issue = issue.unwrap();

                let issue_key = issue.get("key").and_then(|v| v.as_str()).unwrap_or("");
                if issue_key.is_empty() {
                    return Err("missing_issue_key".to_string());
                }

                let fields = issue.get("fields").and_then(|v| v.as_object());
                if fields.is_none() {
                    return Err("missing_fields_object".to_string());
                }
                let fields = fields.unwrap();

                let labels = fields.get("labels").and_then(|v| v.as_array());
                let mut has_ready = false;
                if let Some(lbls) = labels {
                    for lbl in lbls {
                        if lbl.as_str() == Some("ready_for_development") {
                            has_ready = true;
                            break;
                        }
                    }
                }

                if !has_ready {
                    return Err("missing_ready_for_development_label".to_string());
                }

                let summary = fields.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                let description = fields.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let status = fields.get("status").and_then(|v| v.pointer("/name")).and_then(|v| v.as_str()).unwrap_or("");

                let task_event = serde_json::json!({
                    "source": "jira",
                    "event_type": "issue_updated",
                    "issue_key": issue_key,
                    "summary": summary,
                    "description": description,
                    "status": status,
                });

                let routing_key = format!("jira-{}", issue_key);

                Ok(WebhookRequest::Validated { routing_key, task_event })
            }
            _ => Err("Invalid state transition: Request is already validated.".to_string()),
        }
    }
}

// --- App State ---

struct AppState {
    pulsar_producer: Arc<Mutex<Option<Producer<TokioExecutor>>>>,
}

// --- Handlers ---

async fn health_check(State(state): State<Arc<AppState>>) -> Json<Value> {
    let producer_lock = state.pulsar_producer.lock().await;
    let pulsar_connected = producer_lock.is_some();
    Json(serde_json::json!({
        "status": "healthy",
        "pulsar_connected": pulsar_connected,
    }))
}

async fn publish_to_pulsar(
    producer_arc: Arc<Mutex<Option<Producer<TokioExecutor>>>>,
    routing_key: String,
    task_event: Value,
) {
    let mut producer_lock = producer_arc.lock().await;
    if let Some(producer) = producer_lock.as_mut() {
        let payload_bytes = serde_json::to_vec(&task_event).unwrap_or_default();
        let msg = pulsar::producer::Message {
            payload: payload_bytes,
            partition_key: Some(routing_key.clone()),
            ..Default::default()
        };
        match producer.send_non_blocking(msg).await {
            Ok(rx) => match rx.await {
                Ok(receipt) => {
                    info!("Published message (id: {:?}) with key '{}'", receipt, routing_key);
                }
                Err(e) => {
                    error!("Failed to receive receipt: {}", e);
                }
            },
            Err(e) => {
                error!("Failed to publish message to Pulsar: {}", e);
            }
        }
    } else {
        error!("Pulsar producer is not initialized.");
    }
}

async fn bitbucket_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let mut header_map = HashMap::new();
    for (k, v) in headers.iter() {
        if let Ok(v_str) = v.to_str() {
            header_map.insert(k.as_str().to_lowercase(), v_str.to_string());
        }
    }

    let request = WebhookRequest::new("bitbucket", payload, header_map);
    match request.validate_bitbucket() {
        Ok(WebhookRequest::Validated { routing_key, task_event }) => {
            tokio::spawn(publish_to_pulsar(
                state.pulsar_producer.clone(),
                routing_key.clone(),
                task_event,
            ));
            Json(serde_json::json!({"status": "accepted", "routing_key": routing_key}))
        }
        Ok(_) => {
            Json(serde_json::json!({"status": "error", "reason": "unexpected state"}))
        }
        Err(reason) => {
            info!("Bitbucket event ignored: {}", reason);
            Json(serde_json::json!({"status": "ignored", "reason": reason}))
        }
    }
}

async fn jira_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let mut header_map = HashMap::new();
    for (k, v) in headers.iter() {
        if let Ok(v_str) = v.to_str() {
            header_map.insert(k.as_str().to_lowercase(), v_str.to_string());
        }
    }

    let request = WebhookRequest::new("jira", payload, header_map);
    match request.validate_jira() {
        Ok(WebhookRequest::Validated { routing_key, task_event }) => {
            tokio::spawn(publish_to_pulsar(
                state.pulsar_producer.clone(),
                routing_key.clone(),
                task_event,
            ));
            Json(serde_json::json!({"status": "accepted", "routing_key": routing_key}))
        }
        Ok(_) => {
            Json(serde_json::json!({"status": "error", "reason": "unexpected state"}))
        }
        Err(reason) => {
            info!("Jira event ignored: {}", reason);
            Json(serde_json::json!({"status": "ignored", "reason": reason}))
        }
    }
}

// --- Pub/Sub Listener ---

async fn run_pubsub_listener(producer_arc: Arc<Mutex<Option<Producer<TokioExecutor>>>>) {
    let _project_id = env::var("GCP_PROJECT_ID").unwrap_or_else(|_| "ostk-gcp-dataengcore-prod".to_string());
    let sub_id = env::var("GCP_SUBSCRIPTION_ID").unwrap_or_else(|_| "test-jira-sub".to_string());

    let config = PubSubConfig::default().with_auth().await;
    match config {
        Ok(config) => {
            if let Ok(client) = PubSubClient::new(config).await {
                let subscription = client.subscription(&sub_id);
                info!("Listening for GCP Pub/Sub messages on {}...", sub_id);
                
                // Keep streaming
                use futures::StreamExt;
                if let Ok(mut stream) = subscription.subscribe(None).await {
                    while let Some(message) = stream.next().await {
                        let msg_data = message.message.data.clone();
                        if let Ok(payload_str) = String::from_utf8(msg_data) {
                            if let Ok(payload) = serde_json::from_str::<Value>(&payload_str) {
                                let request = WebhookRequest::new("pubsub_jira", payload, HashMap::new());
                                match request.validate_pubsub_jira() {
                                    Ok(WebhookRequest::Validated { routing_key, task_event }) => {
                                        info!("Successfully bridged Pub/Sub message for {} to Pulsar.", routing_key);
                                        publish_to_pulsar(
                                            producer_arc.clone(),
                                            routing_key,
                                            task_event,
                                        ).await;
                                    }
                                    Ok(_) => {
                                        error!("Unexpected state after pubsub validation");
                                    }
                                    Err(reason) => {
                                        info!("Ignoring Pub/Sub Jira event: {}", reason);
                                    }
                                }
                            } else {
                                error!("Failed to decode Pub/Sub message data as JSON.");
                            }
                        }
                        let _ = message.ack().await;
                    }
                } else {
                    error!("Failed to subscribe to Pub/Sub stream");
                }
            } else {
                error!("Failed to create Pub/Sub client");
            }
        }
        Err(e) => {
            error!("Failed to initialize GCP Pub/Sub config. Is ADC configured? Error: {}", e);
        }
    }
}

// --- Main ---

#[tokio::main]
async fn main() {
    // Configure Logging
    tracing_subscriber::fmt::init();
    info!("Starting Opencode Webhook Ingester");

    // Environment Variables Configuration
    let pulsar_service_url = env::var("PULSAR_SERVICE_URL").unwrap_or_else(|_| "pulsar://localhost:6650".to_string());
    let pulsar_topic = env::var("PULSAR_TOPIC").unwrap_or_else(|_| "persistent://public/default/agent-tasks".to_string());
    let pulsar_auth_token = env::var("PULSAR_AUTH_TOKEN").ok();

    let mut builder = Pulsar::builder(pulsar_service_url.clone(), TokioExecutor)
        .with_allow_insecure_connection(true);
    if let Some(token) = pulsar_auth_token {
        builder = builder.with_auth(Authentication {
            name: "token".to_string(),
            data: token.into_bytes(),
        });
    }

    let pulsar_producer_arc = Arc::new(Mutex::new(None));

    match builder.build().await {
        Ok(pulsar) => {
            match pulsar
                .producer()
                .with_topic(pulsar_topic.clone())
                .build()
                .await
            {
                Ok(producer) => {
                    info!("Successfully connected to Pulsar and initialized producer on topic: {}", pulsar_topic);
                    let mut lock = pulsar_producer_arc.lock().await;
                    *lock = Some(producer);
                }
                Err(e) => error!("Failed to initialize Pulsar producer: {}", e),
            }
        }
        Err(e) => error!("Failed to initialize Pulsar client: {}", e),
    }

    // Start Pub/Sub Listener thread
    let producer_clone = pulsar_producer_arc.clone();
    tokio::spawn(async move {
        run_pubsub_listener(producer_clone).await;
    });

    let app_state = Arc::new(AppState {
        pulsar_producer: pulsar_producer_arc,
    });

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/webhook/bitbucket", post(bitbucket_webhook))
        .route("/webhook/jira", post(jira_webhook))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    info!("Listening on 0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}
