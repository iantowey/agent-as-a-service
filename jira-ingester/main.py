import os
import json
import base64
import logging
from flask import Request
from google.cloud import pubsub_v1

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# Environment variables
PROJECT_ID = os.environ.get("PROJECT_ID", "ostk-gcp-dataengcore-prod")
TOPIC_ID = os.environ.get("TOPIC_ID", "jira-webhooks")

# Initialize Pub/Sub client globally for connection pooling
publisher = pubsub_v1.PublisherClient()
topic_path = publisher.topic_path(PROJECT_ID, TOPIC_ID)

def ingest_jira_webhook(request: Request):
    """
    HTTP Cloud Function that receives Jira webhooks and publishes them to Pub/Sub.
    """
    try:
        # Get the JSON payload from the request
        request_json = request.get_json(silent=True)
        if not request_json:
            logger.warning("No JSON payload received or invalid JSON")
            return "Bad Request: No JSON payload", 400

        # Log basic info about the event
        webhook_event = request_json.get("webhookEvent", "unknown")
        issue = request_json.get("issue", {})
        issue_key = issue.get("key", "unknown")
        logger.info(f"Received Jira webhook event: {webhook_event} for issue: {issue_key}")

        # Convert the payload to a JSON string and encode as bytes
        message_data = json.dumps(request_json).encode("utf-8")

        # Publish the message to Pub/Sub
        # We use the issue key as the ordering key/attribute if needed by downstream
        future = publisher.publish(
            topic_path,
            data=message_data,
            source="jira",
            event_type=webhook_event,
            issue_key=issue_key
        )
        
        # Wait for the publish call to complete
        message_id = future.result()
        logger.info(f"Successfully published message {message_id} to {TOPIC_ID}")

        return "OK", 200

    except Exception as e:
        logger.error(f"Error processing webhook: {str(e)}", exc_info=True)
        # Return 500 so Jira knows the webhook failed and can retry if configured
        return f"Internal Server Error: {str(e)}", 500
