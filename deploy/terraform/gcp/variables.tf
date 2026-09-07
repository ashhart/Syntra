// Input variables for the Syntra GCP deployment.
// Single Cloud Run service, single Filestore tier — no fleet.

variable "project_id" {
  description = "GCP project ID to deploy into."
  type        = string
}

variable "region" {
  description = "GCP region for the Cloud Run service and Filestore instance."
  type        = string
  default     = "europe-west1"
}

variable "image_tag" {
  description = "Tag of ghcr.io/ashhart/syntra to deploy."
  type        = string
  default     = "demo"
}

variable "store_size_gb" {
  description = "Provisioned Filestore size in GiB. Note: Filestore BASIC_HDD has a 1024 GiB minimum charged to the bill, but you can request smaller sizes (the smallest the API accepts is documented per-tier). Default 10 here is informational — the Filestore resource below clamps to 1024."
  type        = number
  default     = 10
}

variable "admin_token" {
  description = "Bearer token Syntra will accept on /admin/* routes. Surfaced as SYNTRA_ADMIN_KEY inside the container."
  type        = string
  sensitive   = true
}

variable "allowed_cidrs" {
  description = "CIDR ranges allowed to reach the load balancer / Cloud Run service. Default 0.0.0.0/0. When using Cloud Run domain mapping the service is always publicly reachable; this is enforced via a Cloud Armor policy attached to the load balancer (only relevant if domain_name is set)."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "domain_name" {
  description = "Public DNS name (e.g. syntra.example.com). If set, the module creates a Cloud Run domain mapping which provisions a Google-managed TLS cert automatically. If empty the service is reachable only via its *.run.app URL (which is also HTTPS, via Google's wildcard cert)."
  type        = string
  default     = ""
}
