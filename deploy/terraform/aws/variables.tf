// Input variables for the Syntra AWS deployment.
// Surface is intentionally small — this module deploys ONE container
// with a shared admin token, not a fleet.

variable "region" {
  description = "AWS region to deploy into."
  type        = string
  default     = "eu-west-1"
}

variable "image_tag" {
  description = "Tag of ghcr.io/ashhart/syntra to deploy."
  type        = string
  default     = "demo"
}

variable "store_size_gb" {
  description = "Informational size hint for the persistent store. EFS is elastic so this is not a hard cap; left here for symmetry with the GCP/Azure modules and so you can wire it to a CloudWatch alarm if you want."
  type        = number
  default     = 10
}

variable "admin_token" {
  description = "Bearer token Syntra will accept on /admin/* routes. Surfaced as SYNTRA_ADMIN_KEY inside the container."
  type        = string
  sensitive   = true
}

variable "allowed_cidrs" {
  description = "CIDR ranges allowed to reach the ALB. Default 0.0.0.0/0 (public). Tighten this for an internal-only demo."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "domain_name" {
  description = "Public DNS name to attach to the ALB (e.g. syntra.example.com). If set, route53_zone_name must also be set and the zone must already exist in this account. Leave empty to skip ACM/TLS and serve over plain HTTP on the ALB DNS name."
  type        = string
  default     = ""
}

variable "route53_zone_name" {
  description = "Route53 hosted zone name (e.g. example.com). Required if domain_name is set."
  type        = string
  default     = ""
}
