// Input variables for the Syntra Azure deployment.

variable "region" {
  description = "Azure region for all resources. (Aliased as `location` in some Azure docs; keeping the name `region` for parity with the AWS/GCP modules.)"
  type        = string
  default     = "westeurope"
}

variable "resource_group_name" {
  description = "Resource group to create and place every Syntra resource into."
  type        = string
  default     = "syntra-rg"
}

variable "image_tag" {
  description = "Tag of ghcr.io/ashhart/syntra to deploy."
  type        = string
  default     = "demo"
}

variable "store_size_gb" {
  description = "Provisioned size of the Azure Files share in GiB. Standard SMB shares can be 100 GiB to multiple TiB; the share quota acts as a soft cap on consumption."
  type        = number
  default     = 10
}

variable "admin_token" {
  description = "Bearer token Syntra will accept on /admin/* routes. Surfaced as SYNTRA_ADMIN_KEY inside the container and stored as a Container App secret."
  type        = string
  sensitive   = true
}

variable "allowed_cidrs" {
  description = "CIDR ranges allowed to reach the Application Gateway. Default 0.0.0.0/0 (public)."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "domain_name" {
  description = "Public DNS name for the App Gateway (e.g. syntra.example.com). If set, an App Gateway managed certificate is provisioned for it. If empty, the App Gateway listens on its public IP without TLS."
  type        = string
  default     = ""
}
