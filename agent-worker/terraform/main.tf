terraform {
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 5.0"
    }
    local = {
      source  = "hashicorp/local"
      version = "~> 2.0"
    }
  }
}

variable "project_id" {
  description = "The GCP project ID where the SA lives"
  type        = string
  default     = "ostk-gcp-dataengcore-prod"
}

variable "vertex_project_id" {
  description = "The GCP project ID where Vertex AI is used"
  type        = string
  default     = "ostk-gcp-geminicodeassist-test"
}

provider "google" {
  project               = var.project_id
  region                = "us-central1"
  billing_project       = var.project_id
  user_project_override = true
}

# Provider for the Vertex AI project
provider "google" {
  alias                 = "vertex"
  project               = var.vertex_project_id
  region                = "us-central1"
  billing_project       = var.project_id
  user_project_override = true
}

# 1. Create the Service Account
resource "google_service_account" "agent_worker_sa" {
  account_id   = "opencode-agent-worker"
  display_name = "Opencode Agent Worker Service Account"
}

# 2. Grant the SA access to Vertex AI in the target project
resource "google_project_iam_member" "vertex_ai_user" {
  provider = google.vertex
  project  = var.vertex_project_id
  role     = "roles/aiplatform.user"
  member   = "serviceAccount:${google_service_account.agent_worker_sa.email}"
}

# Also grant permission to use the project for billing/quota (Service Usage Consumer)
resource "google_project_iam_member" "service_usage_consumer" {
  provider = google.vertex
  project  = var.vertex_project_id
  role     = "roles/serviceusage.serviceUsageConsumer"
  member   = "serviceAccount:${google_service_account.agent_worker_sa.email}"
}

# 3. Create a JSON Key for the Service Account
resource "google_service_account_key" "agent_worker_sa_key" {
  service_account_id = google_service_account.agent_worker_sa.name
  public_key_type    = "TYPE_X509_PEM_FILE"
}

# 4. Save the JSON Key to a local file
resource "local_file" "sa_key_file" {
  content  = base64decode(google_service_account_key.agent_worker_sa_key.private_key)
  filename = "${path.module}/agent-worker-sa-key.json"
}

output "service_account_email" {
  value = google_service_account.agent_worker_sa.email
}
