import os
import json
import logging
import threading
from typing import Dict, Any, Optional
from fastapi import FastAPI, Request, HTTPException, BackgroundTasks
import pulsar
from google.cloud import pubsub_v1

# Configure Logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
logger = logging.getLogger("webhook-ingester")

# Environment Variables Configuration
PULSAR_SERVICE_URL = os.environ.get("PULSAR_SERVICE_URL", "pulsar://localhost:6650")
PULSAR_TOPIC = os.environ.get("PULSAR_TOPIC", "persistent://public/default/agent-tasks")
PULSAR_AUTH_TOKEN = os.environ.get("PULSAR_AUTH_TOKEN") # Optional

# GCP Pub/Sub Config
GCP_PROJECT_ID = os.environ.get("GCP_PROJECT_ID", "ostk-gcp-dataengcore-prod")
GCP_SUBSCRIPTION_ID = os.environ.get("GCP_SUBSCRIPTION_ID", "test-jira-sub")

# Initialize FastAPI App
app = FastAPI(title="Opencode Webhook Ingester", description="Ingests webhooks from Jira (via Pub/Sub) and Bitbucket (direct), publishes to Pulsar")

# Global Clients
pulsar_client: Optional[pulsar.Client] = None
pulsar_producer: Optional[pulsar.Producer] = None
pubsub_subscriber = None
pubsub_streaming_pull_future = None

def pubsub_callback(message: pubsub_v1.subscriber.message.Message):
    """Callback for GCP Pub/Sub messages."""
    try:
        payload_str = message.data.decode('utf-8')
        payload = json.loads(payload_str)
        
        issue = payload.get("issue", {})
        issue_key = issue.get("key")
        fields = issue.get("fields", {})

        if not issue_key:
            logger.warning("Pub/Sub Jira payload missing issue key.")
            return

        # Check if the issue has the required label for development
        labels = fields.get("labels", [])
        if "ready_for_development" not in labels:
            logger.info(f"Ignoring issue {issue_key} as it does not have the 'ready_for_development' label. Labels found: {labels}")
            return

        logger.info(f"Received Pub/Sub Jira Event for issue: {issue_key}")

        task_event = {
            "source": "jira",
            "event_type": "issue_updated", # Defaulting since webhookEvent is not in payload
            "issue_key": issue_key,
            "summary": fields.get("summary", ""),
            "description": fields.get("description", ""),
            "status": fields.get("status", {}).get("name", "")
        }

        routing_key = f"jira-{issue_key}"
        publish_to_pulsar(routing_key, task_event)
        logger.info(f"Successfully bridged Pub/Sub message for {issue_key} to Pulsar.")

    except json.JSONDecodeError:
        logger.error("Failed to decode Pub/Sub message data as JSON.")
    except Exception as e:
        logger.error(f"Error processing Pub/Sub message: {e}")
    finally:
        # Acknowledge the message so it's not delivered again
        message.ack()

def start_pubsub_listener():
    """Starts the GCP Pub/Sub streaming pull in a background thread."""
    global pubsub_subscriber, pubsub_streaming_pull_future
    try:
        pubsub_subscriber = pubsub_v1.SubscriberClient()
        subscription_path = pubsub_subscriber.subscription_path(GCP_PROJECT_ID, GCP_SUBSCRIPTION_ID)
        
        logger.info(f"Listening for GCP Pub/Sub messages on {subscription_path}...")
        pubsub_streaming_pull_future = pubsub_subscriber.subscribe(subscription_path, callback=pubsub_callback)
    except Exception as e:
        logger.error(f"Failed to initialize GCP Pub/Sub subscriber. Is ADC (credentials) configured? Error: {e}")

@app.on_event("startup")
def startup_event():
    global pulsar_client, pulsar_producer
    try:
        auth = pulsar.AuthenticationToken(PULSAR_AUTH_TOKEN) if PULSAR_AUTH_TOKEN else None
        logger.info(f"Connecting to Pulsar at {PULSAR_SERVICE_URL}...")
        pulsar_client = pulsar.Client(PULSAR_SERVICE_URL, authentication=auth, tls_allow_insecure_connection=True)
        
        # Configure producer with message routing for Key_Shared subscriptions
        pulsar_producer = pulsar_client.create_producer(
            PULSAR_TOPIC,
            message_routing_mode=pulsar.PartitionsRoutingMode.CustomPartition
        )
        logger.info(f"Successfully connected to Pulsar and initialized producer on topic: {PULSAR_TOPIC}")
    except Exception as e:
        logger.error(f"Failed to initialize Pulsar client/producer: {e}")
        
    # Start Pub/Sub Listener thread
    t = threading.Thread(target=start_pubsub_listener, daemon=True)
    t.start()

@app.on_event("shutdown")
def shutdown_event():
    global pulsar_client, pubsub_streaming_pull_future
    if pubsub_streaming_pull_future:
        logger.info("Canceling Pub/Sub listener...")
        pubsub_streaming_pull_future.cancel()
    if pulsar_client:
        logger.info("Closing Pulsar client...")
        pulsar_client.close()

def publish_to_pulsar(routing_key: str, event_payload: Dict[str, Any]):
    """Publishes the task event to Pulsar using the routing_key for Key_Shared subscriptions."""
    if not pulsar_producer:
        logger.error("Pulsar producer is not initialized. Cannot publish message.")
        return

    try:
        # Encode payload to JSON bytes
        data = json.dumps(event_payload).encode('utf-8')
        
        # Send message with partition key to ensure messages with the same key
        # (e.g., same PR) go to the same consumer in a Key_Shared subscription.
        msg_id = pulsar_producer.send(
            data,
            partition_key=routing_key
        )
        logger.info(f"Published message {msg_id} with key '{routing_key}' to topic {PULSAR_TOPIC}")
    except Exception as e:
        logger.error(f"Failed to publish message to Pulsar: {e}")

@app.post("/webhook/bitbucket")
async def bitbucket_webhook(request: Request, background_tasks: BackgroundTasks):
    """
    Ingests Bitbucket Webhooks (e.g., repo:refs_changed, pullrequest:comment_created)
    """
    try:
        payload = await request.json()
        event_key = request.headers.get("X-Event-Key", "unknown")
        logger.info(f"Received Bitbucket Event: {event_key}")

        # Example: Filter for PR comments
        if event_key == "pullrequest:comment_created":
            comment_text = payload.get("comment", {}).get("text", "")
            author_name = payload.get("actor", {}).get("name", "")
            
            # Anti-Loop Protection: Ignore comments from the agent itself
            if author_name == "Opencode Agent" or "<<OPENCODE>>" in comment_text:
                logger.info(f"Ignored comment to prevent looping. Author: {author_name}")
                return {"status": "ignored", "reason": "anti_loop"}
            
            pr_info = payload.get("pullrequest", {})
            pr_id = str(pr_info.get("id"))
            repo_info = payload.get("repository", {})
            repo_slug = repo_info.get("slug")
            project_key = repo_info.get("project", {}).get("key")
            branch_name = pr_info.get("source", {}).get("branch", {}).get("name")

            if not all([pr_id, repo_slug, project_key]):
                logger.warning("Payload missing required PR or Repo fields.")
                return {"status": "ignored", "reason": "missing_fields"}

            # Construct the normalized internal task event
            task_event = {
                "source": "bitbucket",
                "event_type": "pr_comment",
                "project_key": project_key,
                "repository_slug": repo_slug,
                "pr_id": pr_id,
                "branch_name": branch_name,
                "comment_text": comment_text,
                "author": payload.get("actor", {}).get("name")
            }

            # Use PR ID or Branch as the routing key to serialize work for this PR
            routing_key = f"{project_key}-{repo_slug}-pr-{pr_id}"

            # Publish async so we return 200 OK to Bitbucket immediately
            background_tasks.add_task(publish_to_pulsar, routing_key, task_event)
            return {"status": "accepted", "routing_key": routing_key}

        # Add other event types here (e.g., pullrequest:created)

        return {"status": "ignored", "reason": f"unhandled_event_type: {event_key}"}

    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="Invalid JSON payload")
    except Exception as e:
        logger.error(f"Error processing Bitbucket webhook: {e}")
        raise HTTPException(status_code=500, detail="Internal Server Error")

@app.post("/webhook/jira")
async def jira_webhook(request: Request, background_tasks: BackgroundTasks):
    """
    Ingests Jira Webhooks (e.g., jira:issue_created, jira:issue_updated)
    """
    try:
        payload = await request.json()
        webhook_event = payload.get("webhookEvent", "unknown")
        logger.info(f"Received Jira Event: {webhook_event}")

        if webhook_event in ["jira:issue_created", "jira:issue_updated"]:
            issue = payload.get("issue", {})
            issue_key = issue.get("key")
            fields = issue.get("fields", {})
            
            # Example: Check if it's assigned to our agent
            # assignee = fields.get("assignee", {}).get("accountId")
            # if assignee == EXPECTED_AGENT_ID:

            if not issue_key:
                return {"status": "ignored", "reason": "no_issue_key"}

            task_event = {
                "source": "jira",
                "event_type": webhook_event.split(":")[-1],
                "issue_key": issue_key,
                "summary": fields.get("summary", ""),
                "description": fields.get("description", ""),
                "status": fields.get("status", {}).get("name", "")
            }

            routing_key = f"jira-{issue_key}"
            background_tasks.add_task(publish_to_pulsar, routing_key, task_event)
            return {"status": "accepted", "routing_key": routing_key}

        return {"status": "ignored", "reason": f"unhandled_event_type: {webhook_event}"}

    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="Invalid JSON payload")
    except Exception as e:
        logger.error(f"Error processing Jira webhook: {e}")
        raise HTTPException(status_code=500, detail="Internal Server Error")

@app.get("/health")
def health_check():
    """Liveness probe for K8s"""
    return {"status": "healthy", "pulsar_connected": pulsar_client is not None}
