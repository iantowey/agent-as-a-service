import os
import json
import logging
import uuid
import time
import pulsar
from kubernetes import client, config
from kubernetes.client.rest import ApiException

# Configure Logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
logger = logging.getLogger("agent-orchestrator")

# Environment Variables
PULSAR_SERVICE_URL = os.environ.get("PULSAR_SERVICE_URL", "pulsar://localhost:6650")
PULSAR_TOPIC = os.environ.get("PULSAR_TOPIC", "persistent://public/default/agent-tasks")
PULSAR_AUTH_TOKEN = os.environ.get("PULSAR_AUTH_TOKEN")
PULSAR_SUBSCRIPTION = os.environ.get("PULSAR_SUBSCRIPTION", "agent-orchestrator-sub")

K8S_NAMESPACE = os.environ.get("K8S_NAMESPACE", "dedev")
AGENT_WORKER_IMAGE = os.environ.get("AGENT_WORKER_IMAGE", "your-registry.com/dedev/opencode-worker:latest")
SECRET_NAME = os.environ.get("SECRET_NAME", "opencode-agent-secrets")

# Initialize K8s Client
try:
    # Attempt to load in-cluster config (when running inside a pod)
    config.load_incluster_config()
    logger.info("Loaded in-cluster Kubernetes configuration.")
except config.ConfigException:
    # Fallback to local kubeconfig for local testing
    config.load_kube_config()
    logger.info("Loaded local kubeconfig.")

batch_v1 = client.BatchV1Api()

def create_k8s_job(task_event: dict) -> bool:
    """Dynamically provisions a Kubernetes Job to execute the Opencode agent."""
    
    event_source = task_event.get("source") # 'bitbucket' or 'jira'
    
    # Construct a unique, valid K8s job name (must match ^[a-z0-9]([-a-z0-9]*[a-z0-9])?$)
    job_id = str(uuid.uuid4())[:8]
    job_name = f"opencode-worker-{event_source}-{job_id}"
    
    # 1. Define the Environment Variables for the Agent
    # We pass the context via env vars, and mount the secrets
    env_vars = [
        client.V1EnvVar(name="EVENT_SOURCE", value=event_source),
        client.V1EnvVar(name="EVENT_TYPE", value=task_event.get("event_type")),
        
        # GCS Root for embeddings
        client.V1EnvVar(name="GCS_DEDEV_ROOT", value=os.environ.get("GCS_DEDEV_ROOT", "my-dedev-gcs-bucket/embeddings")),
        
        # Inject secrets explicitly into the worker
        client.V1EnvVar(name="BITBUCKET_TOKEN", value_from=client.V1EnvVarSource(
            secret_key_ref=client.V1SecretKeySelector(name=SECRET_NAME, key="bitbucket-token")
        )),
        client.V1EnvVar(name="JIRA_TOKEN", value_from=client.V1EnvVarSource(
            secret_key_ref=client.V1SecretKeySelector(name=SECRET_NAME, key="jira-token")
        )),
        client.V1EnvVar(name="JIRA_EMAIL", value_from=client.V1EnvVarSource(
            secret_key_ref=client.V1SecretKeySelector(name=SECRET_NAME, key="jira-email")
        )),
        client.V1EnvVar(name="GEMINI_API_KEY", value_from=client.V1EnvVarSource(
            secret_key_ref=client.V1SecretKeySelector(name=SECRET_NAME, key="gemini-api-key")
        )),
        client.V1EnvVar(name="TEAMS_WEBHOOK_URL", value_from=client.V1EnvVarSource(
            secret_key_ref=client.V1SecretKeySelector(name=SECRET_NAME, key="teams-webhook")
        )),
        client.V1EnvVar(name="GOOGLE_APPLICATION_CREDENTIALS", value="/var/secrets/google/credentials.json")
    ]

    # Specific variables based on the event source
    if event_source == "bitbucket":
        env_vars.extend([
            client.V1EnvVar(name="PROJECT_KEY", value=task_event.get("project_key")),
            client.V1EnvVar(name="REPO_SLUG", value=task_event.get("repository_slug")),
            client.V1EnvVar(name="PR_ID", value=task_event.get("pr_id")),
            client.V1EnvVar(name="BRANCH_NAME", value=task_event.get("branch_name")),
            client.V1EnvVar(name="COMMENT_TEXT", value=task_event.get("comment_text", ""))
        ])
    elif event_source == "jira":
        env_vars.extend([
            client.V1EnvVar(name="ISSUE_KEY", value=task_event.get("issue_key")),
            client.V1EnvVar(name="SUMMARY", value=task_event.get("summary")),
            client.V1EnvVar(name="DESCRIPTION", value=task_event.get("description", ""))
        ])

    # 2. Define the Agent Container
    agent_container = client.V1Container(
        name="opencode-agent",
        image=AGENT_WORKER_IMAGE,
        image_pull_policy="IfNotPresent",
        env=env_vars,
        volume_mounts=[
            client.V1VolumeMount(
                name="custom-tools-volume",
                mount_path="/root/.config/opencode/tools",
                read_only=True
            ),
            client.V1VolumeMount(
                name="gcp-sa-volume",
                mount_path="/var/secrets/google",
                read_only=True
            ),
            # Share the workspace so DinD can see the cloned files
            client.V1VolumeMount(
                name="workspace-volume",
                mount_path="/workspace"
            ),
            # Share the Docker socket from the DinD sidecar
            client.V1VolumeMount(
                name="dind-socket-volume",
                mount_path="/var/run"
            )
        ]
    )

    # 2.5 Define the Docker-in-Docker Sidecar
    dind_container = client.V1Container(
        name="dind-daemon",
        image="docker:dind",
        security_context=client.V1SecurityContext(privileged=True),
        env=[
            # Use the shared /var/run directory for the socket
            client.V1EnvVar(name="DOCKER_HOST", value="unix:///var/run/docker.sock")
        ],
        volume_mounts=[
            client.V1VolumeMount(
                name="workspace-volume",
                mount_path="/workspace"
            ),
            client.V1VolumeMount(
                name="dind-socket-volume",
                mount_path="/var/run"
            )
        ]
    )

    # 3. Define the Volumes
    volumes = [
        client.V1Volume(
            name="custom-tools-volume",
            config_map=client.V1ConfigMapVolumeSource(
                name="opencode-custom-tools",
                default_mode=0o755
            )
        ),
        client.V1Volume(
            name="gcp-sa-volume",
            secret=client.V1SecretVolumeSource(
                secret_name="agent-worker-gcp-sa"
            )
        ),
        client.V1Volume(
            name="workspace-volume",
            empty_dir=client.V1EmptyDirVolumeSource()
        ),
        client.V1Volume(
            name="dind-socket-volume",
            empty_dir=client.V1EmptyDirVolumeSource()
        )
    ]

    # 4. Define the Pod Template
    template = client.V1PodTemplateSpec(
        metadata=client.V1ObjectMeta(labels={"app": "opencode-worker", "job_id": job_id}),
        spec=client.V1PodSpec(
            restart_policy="Never",
            containers=[agent_container, dind_container],
            volumes=volumes
        )
    )

    # 4. Define the Job
    job = client.V1Job(
        api_version="batch/v1",
        kind="Job",
        metadata=client.V1ObjectMeta(name=job_name, namespace=K8S_NAMESPACE),
        spec=client.V1JobSpec(
            template=template,
            backoff_limit=0, # Do not retry if the agent fails (e.g. build failure)
            active_deadline_seconds=1800, # Hard timeout: Kill the pod after 30 minutes to prevent runaway LLM costs
            ttl_seconds_after_finished=3600 # Clean up the job 1 hour after it finishes
        )
    )

    # 5. Submit to K8s API
    try:
        logger.info(f"Submitting K8s Job: {job_name}")
        api_response = batch_v1.create_namespaced_job(
            body=job,
            namespace=K8S_NAMESPACE
        )
        logger.info(f"Job created successfully. UID: {api_response.metadata.uid}")
        return True
    except ApiException as e:
        logger.error(f"Exception when calling BatchV1Api->create_namespaced_job: {e}")
        return False

def main():
    logger.info("Initializing Agent Orchestrator...")
    
    auth = pulsar.AuthenticationToken(PULSAR_AUTH_TOKEN) if PULSAR_AUTH_TOKEN else None
    client_instance = pulsar.Client(PULSAR_SERVICE_URL, authentication=auth, tls_allow_insecure_connection=True)

    # Create consumer with Key_Shared subscription
    # This guarantees that messages with the same partition_key (e.g. same PR)
    # are routed sequentially to the same consumer, preventing merge conflicts.
    consumer = client_instance.subscribe(
        PULSAR_TOPIC,
        subscription_name=PULSAR_SUBSCRIPTION,
        consumer_type=pulsar.ConsumerType.KeyShared,
        initial_position=pulsar.InitialPosition.Earliest
    )

    logger.info(f"Listening for tasks on {PULSAR_TOPIC} using KeyShared subscription...")

    try:
        while True:
            msg = consumer.receive()
            try:
                payload_str = msg.data().decode('utf-8')
                task_event = json.loads(payload_str)
                routing_key = msg.partition_key()
                
                logger.info(f"Received task with routing_key '{routing_key}': {payload_str}")

                # Provision the K8s Job
                success = create_k8s_job(task_event)
                
                if success:
                    # Acknowledge the message ONLY if the K8s job was successfully scheduled
                    consumer.acknowledge(msg)
                    logger.info("Message acknowledged.")
                else:
                    # If scheduling fails (e.g., API server down), negatively acknowledge
                    # so Pulsar delivers it again later.
                    consumer.negative_acknowledge(msg)
                    logger.warning("Failed to schedule job. Message negatively acknowledged.")

            except Exception as e:
                logger.error(f"Error processing message: {e}")
                # Depending on the error (e.g., bad JSON format), you might want to ack
                # it to remove poison pills, or send it to a Dead Letter Queue.
                consumer.acknowledge(msg) 

    except KeyboardInterrupt:
        logger.info("Shutting down orchestrator...")
    finally:
        consumer.close()
        client_instance.close()

if __name__ == "__main__":
    main()