// Public URL Syntra is reachable on. With a domain → HTTPS via ACM;
// without a domain → plain HTTP at the ALB's amazonaws.com DNS name.
output "endpoint_url" {
  description = "Public URL for the Syntra API."
  value = var.domain_name != "" ? "https://${var.domain_name}" : "http://${aws_lb.this.dns_name}"
}

output "alb_dns_name" {
  description = "Raw ALB DNS name. CNAME this from your own DNS if you don't use Route53."
  value       = aws_lb.this.dns_name
}

output "efs_id" {
  description = "EFS file system ID backing /store."
  value       = aws_efs_file_system.store.id
}
