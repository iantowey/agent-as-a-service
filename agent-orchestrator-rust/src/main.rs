use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    ConfigMapVolumeSource, Container, EmptyDirVolumeSource, EnvVar, EnvVarSource, PodSpec,
    PodTemplateSpec, SecretKeySelector, SecretVolumeSource, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{
    api::{Api, PostParams},
    Client,
};
use pulsar::{
    consumer::InitialPosition, Authentication, Consumer, ConsumerOptions, Pulsar, SubType,
    TokioExecutor,
};
use serde_json::Value;
use std::env;
use tracing::{error, info, warn};
use uuid::Uuid;

// --- State Management ---
// Following the ADT Pattern

pub enum OrchestratorState {
    Received {
        payload_str: String,
        routing_key: Option<String>,
        msg: pulsar::consumer::Message<Vec<u8>>,
    },
    Parsed {
        task_event: Value,
        msg: pulsar::consumer::Message<Vec<u8>>,
    },
    JobCreated {
        msg: pulsar::consumer::Message<Vec<u8>>,
    },
    Failed {
        error: String,
        msg: pulsar::consumer::Message<Vec<u8>>,
    },
}

impl OrchestratorState {
    pub fn new(msg: pulsar::consumer::Message<Vec<u8>>) -> Self {
        let payload_str = String::from_utf8_lossy(&msg.payload.data).to_string();
        let routing_key = msg.payload.metadata.partition_key.clone();
        OrchestratorState::Received {
            payload_str,
            routing_key,
            msg,
        }
    }

    pub fn parse(self) -> Self {
        match self {
            OrchestratorState::Received {
                payload_str, msg, ..
            } => match serde_json::from_str::<Value>(&payload_str) {
                Ok(task_event) => OrchestratorState::Parsed { task_event, msg },
                Err(e) => OrchestratorState::Failed {
                    error: format!("Failed to parse JSON: {}", e),
                    msg,
                },
            },
            _ => self,
        }
    }

    pub async fn process_job(
        self,
        k8s_client: Client,
        namespace: &str,
        agent_image: &str,
        secret_name: &str,
        gcs_root: &str,
    ) -> Self {
        match self {
            OrchestratorState::Parsed { task_event, msg } => {
                let success = create_k8s_job(
                    k8s_client,
                    &task_event,
                    namespace,
                    agent_image,
                    secret_name,
                    gcs_root,
                )
                .await;

                if success {
                    OrchestratorState::JobCreated { msg }
                } else {
                    OrchestratorState::Failed {
                        error: "Failed to schedule job".to_string(),
                        msg,
                    }
                }
            }
            _ => self,
        }
    }

    pub async fn finalize(self, consumer: &mut Consumer<Vec<u8>, TokioExecutor>) {
        match self {
            OrchestratorState::JobCreated { msg } => {
                if let Err(e) = consumer.ack(&msg).await {
                    error!("Failed to acknowledge message: {}", e);
                } else {
                    info!("Message acknowledged.");
                }
            }
            OrchestratorState::Failed { error, msg } => {
                warn!("Message processing failed: {}", error);
                if error.contains("Failed to schedule job") {
                    if let Err(e) = consumer.nack(&msg).await {
                        error!("Failed to nack message: {}", e);
                    } else {
                        info!("Message negatively acknowledged.");
                    }
                } else {
                    // E.g., unparseable JSON - ack to prevent poison pills
                    if let Err(e) = consumer.ack(&msg).await {
                        error!("Failed to acknowledge unparseable message: {}", e);
                    } else {
                        info!("Unparseable message acknowledged.");
                    }
                }
            }
            _ => {
                warn!("Unexpected state reached in finalize");
            }
        }
    }
}

// --- K8s Job Creation ---

async fn create_k8s_job(
    client: Client,
    task_event: &Value,
    namespace: &str,
    agent_image: &str,
    secret_name: &str,
    gcs_root: &str,
) -> bool {
    let jobs: Api<Job> = Api::namespaced(client, namespace);

    let event_source = task_event
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let job_id = Uuid::new_v4().to_string().chars().take(8).collect::<String>();
    let job_name = format!("opencode-worker-{}-{}", event_source, job_id);

    let mut env_vars = vec![
        EnvVar {
            name: "EVENT_SOURCE".to_string(),
            value: Some(event_source.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "EVENT_TYPE".to_string(),
            value: task_event
                .get("event_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "GCS_DEDEV_ROOT".to_string(),
            value: Some(gcs_root.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "BITBUCKET_TOKEN".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: Some(secret_name.to_string()),
                    key: "bitbucket-token".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "JIRA_TOKEN".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: Some(secret_name.to_string()),
                    key: "jira-token".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "JIRA_EMAIL".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: Some(secret_name.to_string()),
                    key: "jira-email".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "GEMINI_API_KEY".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: Some(secret_name.to_string()),
                    key: "gemini-api-key".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "TEAMS_WEBHOOK_URL".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: Some(secret_name.to_string()),
                    key: "teams-webhook".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        EnvVar {
            name: "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
            value: Some("/var/secrets/google/credentials.json".to_string()),
            ..Default::default()
        },
    ];

    if event_source == "bitbucket" {
        env_vars.extend(vec![
            EnvVar {
                name: "PROJECT_KEY".to_string(),
                value: task_event
                    .get("project_key")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "REPO_SLUG".to_string(),
                value: task_event
                    .get("repository_slug")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "PR_ID".to_string(),
                value: task_event
                    .get("pr_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "BRANCH_NAME".to_string(),
                value: task_event
                    .get("branch_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "COMMENT_TEXT".to_string(),
                value: Some(
                    task_event
                        .get("comment_text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                ),
                ..Default::default()
            },
        ]);
    } else if event_source == "jira" {
        env_vars.extend(vec![
            EnvVar {
                name: "ISSUE_KEY".to_string(),
                value: task_event
                    .get("issue_key")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "SUMMARY".to_string(),
                value: task_event
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "DESCRIPTION".to_string(),
                value: Some(
                    task_event
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                ),
                ..Default::default()
            },
        ]);
    }

    let agent_container = Container {
        name: "opencode-agent".to_string(),
        image: Some(agent_image.to_string()),
        image_pull_policy: Some("IfNotPresent".to_string()),
        env: Some(env_vars),
        volume_mounts: Some(vec![
            VolumeMount {
                name: "custom-tools-volume".to_string(),
                mount_path: "/root/.config/opencode/tools".to_string(),
                read_only: Some(true),
                ..Default::default()
            },
            VolumeMount {
                name: "gcp-sa-volume".to_string(),
                mount_path: "/var/secrets/google".to_string(),
                read_only: Some(true),
                ..Default::default()
            },
            VolumeMount {
                name: "workspace-volume".to_string(),
                mount_path: "/workspace".to_string(),
                ..Default::default()
            },
            VolumeMount {
                name: "dind-socket-volume".to_string(),
                mount_path: "/var/run".to_string(),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };

    let dind_container = Container {
        name: "dind-daemon".to_string(),
        image: Some("docker:dind".to_string()),
        security_context: Some(SecurityContext {
            privileged: Some(true),
            ..Default::default()
        }),
        env: Some(vec![EnvVar {
            name: "DOCKER_HOST".to_string(),
            value: Some("unix:///var/run/docker.sock".to_string()),
            ..Default::default()
        }]),
        volume_mounts: Some(vec![
            VolumeMount {
                name: "workspace-volume".to_string(),
                mount_path: "/workspace".to_string(),
                ..Default::default()
            },
            VolumeMount {
                name: "dind-socket-volume".to_string(),
                mount_path: "/var/run".to_string(),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };

    let volumes = vec![
        Volume {
            name: "custom-tools-volume".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some("opencode-custom-tools".to_string()),
                default_mode: Some(0o755),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "gcp-sa-volume".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some("agent-worker-gcp-sa".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "workspace-volume".to_string(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Default::default()
        },
        Volume {
            name: "dind-socket-volume".to_string(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Default::default()
        },
    ];

    let mut pod_labels = std::collections::BTreeMap::new();
    pod_labels.insert("app".to_string(), "opencode-worker".to_string());
    pod_labels.insert("job_id".to_string(), job_id.clone());

    let template = PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(pod_labels),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            restart_policy: Some("Never".to_string()),
            containers: vec![agent_container, dind_container],
            volumes: Some(volumes),
            ..Default::default()
        }),
    };

    let job = Job {
        metadata: ObjectMeta {
            name: Some(job_name.clone()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            template,
            backoff_limit: Some(0),
            active_deadline_seconds: Some(1800),
            ttl_seconds_after_finished: Some(3600),
            ..Default::default()
        }),
        ..Default::default()
    };

    info!("Submitting K8s Job: {}", job_name);
    match jobs.create(&PostParams::default(), &job).await {
        Ok(api_response) => {
            info!(
                "Job created successfully. UID: {:?}",
                api_response.metadata.uid
            );
            true
        }
        Err(e) => {
            error!("Exception when calling create_namespaced_job: {}", e);
            false
        }
    }
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    info!("Initializing Agent Orchestrator...");

    let pulsar_service_url =
        env::var("PULSAR_SERVICE_URL").unwrap_or_else(|_| "pulsar://localhost:6650".to_string());
    let pulsar_topic = env::var("PULSAR_TOPIC")
        .unwrap_or_else(|_| "persistent://public/default/agent-tasks".to_string());
    let pulsar_auth_token = env::var("PULSAR_AUTH_TOKEN").ok();
    let pulsar_subscription =
        env::var("PULSAR_SUBSCRIPTION").unwrap_or_else(|_| "agent-orchestrator-sub".to_string());

    let k8s_namespace = env::var("K8S_NAMESPACE").unwrap_or_else(|_| "dedev".to_string());
    let agent_worker_image = env::var("AGENT_WORKER_IMAGE")
        .unwrap_or_else(|_| "your-registry.com/dedev/opencode-worker:latest".to_string());
    let secret_name =
        env::var("SECRET_NAME").unwrap_or_else(|_| "opencode-agent-secrets".to_string());
    let gcs_dedev_root =
        env::var("GCS_DEDEV_ROOT").unwrap_or_else(|_| "my-dedev-gcs-bucket/embeddings".to_string());

    // Initialize K8s Client
    let k8s_client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to initialize K8s client: {}", e);
            return Err(e.into());
        }
    };
    info!("Loaded Kubernetes configuration.");

    let mut builder = Pulsar::builder(pulsar_service_url, TokioExecutor)
        .with_allow_insecure_connection(true);

    if let Some(token) = pulsar_auth_token {
        builder = builder.with_auth(Authentication {
            name: "token".to_string(),
            data: token.into_bytes(),
        });
    }

    let pulsar = builder.build().await?;

    let mut consumer: Consumer<Vec<u8>, _> = pulsar
        .consumer()
        .with_topic(pulsar_topic.clone())
        .with_consumer_name("agent-orchestrator")
        .with_subscription_type(SubType::KeyShared)
        .with_subscription(pulsar_subscription)
        .with_options(ConsumerOptions::default().with_initial_position(InitialPosition::Earliest))
        .build()
        .await?;

    info!(
        "Listening for tasks on {} using KeyShared subscription...",
        pulsar_topic
    );

    while let Some(msg) = consumer.next().await {
        match msg {
            Ok(msg) => {
                let state = OrchestratorState::new(msg);

                if let OrchestratorState::Received {
                    ref payload_str,
                    ref routing_key,
                    ..
                } = state
                {
                    info!(
                        "Received task with routing_key '{:?}': {}",
                        routing_key, payload_str
                    );
                }

                state
                    .parse()
                    .process_job(
                        k8s_client.clone(),
                        &k8s_namespace,
                        &agent_worker_image,
                        &secret_name,
                        &gcs_dedev_root,
                    )
                    .await
                    .finalize(&mut consumer)
                    .await;
            }
            Err(e) => {
                error!("Error receiving message from Pulsar: {}", e);
            }
        }
    }

    info!("Shutting down orchestrator...");
    Ok(())
}
