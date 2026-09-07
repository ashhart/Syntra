// Public URL Syntra is reachable on. With a domain → HTTPS via the
// App Gateway managed cert; without a domain → plain HTTP at the
// public IP / DNS label.
output "endpoint_url" {
  description = "Public URL for the Syntra API."
  value = var.domain_name != "" ? "https://${var.domain_name}" : "http://${azurerm_public_ip.appgw.fqdn}"
}

output "appgw_public_ip" {
  description = "Public IP address of the Application Gateway. Point your DNS A-record at this."
  value       = azurerm_public_ip.appgw.ip_address
}

output "appgw_fqdn" {
  description = "Default *.cloudapp.azure.com FQDN assigned to the App Gateway."
  value       = azurerm_public_ip.appgw.fqdn
}

output "container_app_fqdn" {
  description = "Container App ingress FQDN. App Gateway forwards traffic here; you usually shouldn't hit this directly."
  value       = azurerm_container_app.this.ingress[0].fqdn
}

output "file_share_name" {
  description = "Azure Files share backing /store."
  value       = azurerm_storage_share.store.name
}
