terraform {
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 5.0"
    }
    google-beta = {
      source  = "hashicorp/google-beta"
      version = "~> 5.0"
    }
    local = {
      source  = "hashicorp/local"
      version = "~> 2.0"
    }
  }
}

provider "google" {
  project               = var.project_id
  region                = var.region
  billing_project       = var.project_id
  user_project_override = true
}

provider "google-beta" {
  project               = var.project_id
  region                = var.region
  billing_project       = var.project_id
  user_project_override = true
}

# Variables
variable "project_id" {
  description = "The GCP project ID"
  type        = string
  default     = "ostk-gcp-dataengcore-prod"
}

variable "region" {
  description = "The GCP region"
  type        = string
  default     = "us-central1"
}

variable "topic_name" {
  description = "The name of the Pub/Sub topic"
  type        = string
  default     = "jira-webhooks"
}

variable "function_name" {
  description = "The name of the Cloud Function"
  type        = string
  default     = "jira-webhook-ingester"
}

# 1. Create the Pub/Sub Topic
resource "google_pubsub_topic" "jira_webhooks" {
  name = var.topic_name
}

# 2. Archive the Cloud Function source code
data "archive_file" "function_source" {
  type        = "zip"
  source_dir  = "${path.module}/.."
  output_path = "${path.module}/function-source.zip"
  excludes    = ["terraform", ".git", "function-source.zip"]
}

# 3. Create a GCS bucket for the function source code
resource "google_storage_bucket" "function_bucket" {
  name                        = "${var.project_id}-${var.function_name}-source"
  location                    = var.region
  uniform_bucket_level_access = true
}

# 4. Upload the zip file to the bucket
resource "google_storage_bucket_object" "function_zip" {
  name   = "source-${data.archive_file.function_source.output_md5}.zip"
  bucket = google_storage_bucket.function_bucket.name
  source = data.archive_file.function_source.output_path
}

# 5. Create a Service Account for the Cloud Function
resource "google_service_account" "function_sa" {
  account_id   = "${var.function_name}-sa"
  display_name = "Service Account for Jira Webhook Ingester"
}

# 6. Grant the Service Account permission to publish to the Pub/Sub topic
resource "google_pubsub_topic_iam_member" "publisher" {
  topic  = google_pubsub_topic.jira_webhooks.name
  role   = "roles/pubsub.publisher"
  member = "serviceAccount:${google_service_account.function_sa.email}"
}

# 7. Create the Cloud Run Function (Cloud Functions v2)
resource "google_cloudfunctions2_function" "jira_ingester" {
  name        = var.function_name
  location    = var.region
  description = "HTTP function to ingest Jira webhooks and publish to Pub/Sub"

  build_config {
    runtime     = "python310"
    entry_point = "ingest_jira_webhook"
    source {
      storage_source {
        bucket = google_storage_bucket.function_bucket.name
        object = google_storage_bucket_object.function_zip.name
      }
    }
  }

  service_config {
    max_instance_count    = 10
    min_instance_count    = 0
    available_memory      = "256M"
    timeout_seconds       = 60
    service_account_email = google_service_account.function_sa.email
    environment_variables = {
      PROJECT_ID = var.project_id
      TOPIC_ID   = google_pubsub_topic.jira_webhooks.name
    }
  }
}

# 8. Allow unauthenticated invocations (since this is a public webhook endpoint)
# resource "google_cloud_run_service_iam_member" "public_invoker" {
#   location = google_cloudfunctions2_function.jira_ingester.location
#   service  = google_cloudfunctions2_function.jira_ingester.name
#   role     = "roles/run.invoker"
#   member   = "allUsers"
# }

# Outputs
output "webhook_url" {
  description = "The public URL of the Jira Webhook Ingester to configure in Jira"
  value       = google_cloudfunctions2_function.jira_ingester.service_config[0].uri
}

output "pubsub_topic" {
  description = "The name of the Pub/Sub topic where events are published"
  value       = google_pubsub_topic.jira_webhooks.id
}

# =========================================================================
# API Gateway to provide a Permanent API Key (bypassing IAM org policies)
# =========================================================================

# 9. Enable Required APIs for API Gateway & API Keys
resource "google_project_service" "api_gateway" {
  service            = "apigateway.googleapis.com"
  disable_on_destroy = false
}

resource "google_project_service" "service_management" {
  service            = "servicemanagement.googleapis.com"
  disable_on_destroy = false
}

resource "google_project_service" "service_control" {
  service            = "servicecontrol.googleapis.com"
  disable_on_destroy = false
}

resource "google_project_service" "api_keys" {
  service            = "apikeys.googleapis.com"
  disable_on_destroy = false
}

# 10. Grant the Service Account permission to invoke the Cloud Run function
resource "google_cloud_run_service_iam_member" "gateway_invoker" {
  location = google_cloudfunctions2_function.jira_ingester.location
  service  = google_cloudfunctions2_function.jira_ingester.name
  role     = "roles/run.invoker"
  member   = "serviceAccount:${google_service_account.function_sa.email}"
}

# 11. Create the API Gateway API
resource "google_api_gateway_api" "jira_api" {
  provider     = google-beta
  api_id       = "${var.function_name}-api"
  display_name = "Jira Webhook API"
  depends_on   = [google_project_service.api_gateway]
}

# 12. Create the API Config
resource "google_api_gateway_api_config" "jira_api_config" {
  provider      = google-beta
  api           = google_api_gateway_api.jira_api.api_id
  api_config_id = "${var.function_name}-config"

  openapi_documents {
    document {
      path     = "spec.yaml"
      contents = base64encode(templatefile("${path.module}/spec.yaml.tpl", {
        cloud_run_url = google_cloudfunctions2_function.jira_ingester.service_config[0].uri
      }))
    }
  }

  gateway_config {
    backend_config {
      google_service_account = google_service_account.function_sa.email
    }
  }

  depends_on = [google_project_service.service_management]
}

# 13. Create the API Gateway
resource "google_api_gateway_gateway" "jira_gateway" {
  provider   = google-beta
  api_config = google_api_gateway_api_config.jira_api_config.id
  gateway_id = "${var.function_name}-gw"
  region     = var.region
  depends_on = [google_project_service.service_control]
}

# 14. Create an API Key
resource "google_apikeys_key" "jira_api_key" {
  name         = "${var.function_name}-key"
  display_name = "Jira Webhook API Key"

  restrictions {
    api_targets {
      service = google_api_gateway_api.jira_api.managed_service
    }
  }

  depends_on = [google_project_service.api_keys]
}

# 15. Save the API Key to a local file
resource "local_file" "api_key_file" {
  content  = google_apikeys_key.jira_api_key.key_string
  filename = "${path.module}/jira_webhook_token.txt"
}

output "gateway_url" {
  description = "The public URL of the API Gateway to configure in Jira"
  value       = "https://${google_api_gateway_gateway.jira_gateway.default_hostname}/?key=${google_apikeys_key.jira_api_key.key_string}"
  sensitive   = true
}

output "api_key" {
  description = "The permanent API key for the webhook"
  value       = google_apikeys_key.jira_api_key.key_string
  sensitive   = true
}
