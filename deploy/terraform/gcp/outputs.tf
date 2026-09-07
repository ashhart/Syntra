// Public URL Syntra is reachable on. If a custom domain is configured,
// prefer the HTTPS URL with that domain; otherwise return the
// *.run.app URL Cloud Run assigns automatically (which is also HTTPS).
output "endpoint_url" {
  description = "Public URL for the Syntra API."
  value = var.domain_name != "" ? "https://${var.domain_name}" : google_cloud_run_v2_service.this.uri
}

output "cloud_run_url" {
  description = "Default *.run.app URL Cloud Run assigns to the service."
  value       = google_cloud_run_v2_service.this.uri
}

output "filestore_ip" {
  description = "Filestore instance IP address. Useful for debugging mount issues."
  value       = google_filestore_instance.store.networks[0].ip_addresses[0]
}

output "domain_mapping_records" {
  description = "DNS records you must create at your registrar / DNS provider when domain_name is set. Empty list otherwise."
  value       = var.domain_name != "" ? google_cloud_run_domain_mapping.this[0].status[0].resource_records : []
}
