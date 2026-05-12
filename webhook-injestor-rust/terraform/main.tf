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
  description = "The GCP project ID"
  type        = string
  default     = "ostk-gcp-dataengcore-prod"
}

provider "google" {
  project               = var.project_id
  region                = "us-central1"
  billing_project       = var.project_id
  user_project_override = true
}

# 1. Create the Service Account
resource "google_service_account" "ingester_sa" {
  account_id   = "webhook-ingester-app"
  display_name = "Webhook Ingester Service Account"
}

# 2. Grant the SA access to consume from Pub/Sub
resource "google_project_iam_member" "pubsub_subscriber" {
  project = var.project_id
  role    = "roles/pubsub.subscriber"
  member  = "serviceAccount:${google_service_account.ingester_sa.email}"
}

# 3. Create a JSON Key for the Service Account
resource "google_service_account_key" "ingester_sa_key" {
  service_account_id = google_service_account.ingester_sa.name
  public_key_type    = "TYPE_X509_PEM_FILE"
}

# 4. Save the JSON Key to a local file
resource "local_file" "sa_key_file" {
  content  = base64decode(google_service_account_key.ingester_sa_key.private_key)
  filename = "${path.module}/webhook-ingester-sa-key.json"
}

output "service_account_email" {
  value = google_service_account.ingester_sa.email
}
