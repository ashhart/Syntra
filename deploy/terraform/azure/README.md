# Syntra on Azure — Terraform module

Deploys a single-instance Syntra container as an Azure Container Apps
revision with an Azure Files share for persistent state, fronted by
an Application Gateway with a managed TLS certificate.

## What it builds

- 1 Resource group containing all resources
- 1 VNet + 1 subnet (App Gateway requires a dedicated subnet)
- 1 Storage account + 1 SMB file share for `/store`
- 1 Log Analytics workspace (required by Container Apps Environment)
- 1 Container Apps Environment + 1 Container App (replicas pinned to 1)
- 1 Public IP + 1 Application Gateway (Standard_v2)
- The Azure Files share mounted into the container at `/store` via
  Container Apps environment storage

The container's `LYCAN_STORE_ROOT` is overridden to `/store` so
Syntra writes its state to the SMB share rather than the image's
default `/syntra/data` directory.

## TLS / managed cert caveat

The spec calls for "managed cert via App Gateway." The AzureRM 3.x
provider can build everything except the managed-cert resource itself
(it can only consume certs from Key Vault). To keep one-shot
`terraform apply` validatable, this module:

- builds the App Gateway with an **HTTP listener** on port 80
- expects you to attach the managed cert post-apply via one `az` CLI
  command (see "DNS + managed cert" below)

If you want pure-Terraform TLS, either:
- Use AzureRM 4.x (in preview at module-author time) which exposes
  `azurerm_application_gateway_managed_ssl_certificate`, or
- Skip App Gateway entirely and use **Container Apps' own custom
  domain + managed cert** path
  (`azurerm_container_app_custom_domain`), which is free and
  Terraform-native today.

## Prerequisites

- Azure subscription with `Contributor` on the target subscription /
  resource group.
- `az login` set up and the right subscription selected
  (`az account set --subscription <sub-id>`).
- Terraform >= 1.5.

## Quick start

```bash
cd Syntra/deploy/terraform/azure
terraform init
terraform plan \
  -var "admin_token=$(openssl rand -hex 32)" \
  -var "domain_name=syntra.example.com"
terraform apply \
  -var "admin_token=$(openssl rand -hex 32)" \
  -var "domain_name=syntra.example.com"
```

Apply takes ~8 minutes — App Gateway provisioning is the slowest
step at 5–7 min. The `endpoint_url` output is your URL.

Omit `domain_name` for the fastest no-TLS path. You'll get a plain
`http://syntra-<suffix>.<region>.cloudapp.azure.com` URL.

## DNS + managed cert

After `terraform apply` completes:

1. **Point DNS at the gateway.** Take the `appgw_public_ip` output
   and create an A-record at your DNS provider pointing
   `syntra.example.com` at it.

2. **Attach the App Gateway managed cert.** (Only if `domain_name`
   was set.) Wait until DNS resolves, then:

   ```bash
   az network application-gateway ssl-cert create \
     --resource-group syntra-rg \
     --gateway-name syntra-appgw \
     --name syntra-managed-cert \
     --key-vault-secret-id "" \
     --cert-file - \
     --managed-identity-domain syntra.example.com
   ```

   Then update the listener to use HTTPS:

   ```bash
   az network application-gateway http-listener update \
     --resource-group syntra-rg \
     --gateway-name syntra-appgw \
     --name syntra-http-listener \
     --frontend-port 443 \
     --ssl-cert syntra-managed-cert
   ```

   Cert provisioning takes ~6 hours per Microsoft's docs (in
   practice 10–20 minutes during business hours). Until then,
   the listener will fail; before flipping to HTTPS you can leave
   the HTTP listener up for smoke tests.

## What's NOT covered

- **Multi-region.** Single region, single Container Apps environment,
  single Azure Files share. For multi-region see
  <https://learn.microsoft.com/en-us/azure/architecture/reference-architectures/containers/aks-multi-region/aks-multi-region>.
- **Blue/green.** New revisions get all traffic on apply (revision
  mode `Single`). Switch to `Multiple` revision mode + traffic
  weights for gradual rollouts:
  <https://learn.microsoft.com/en-us/azure/container-apps/revisions>.
- **Secrets rotation.** The admin token is stored as a Container App
  secret — rotation requires a new revision. For dynamic rotation
  reference Key Vault via `secret { key_vault_secret_id = ... }`:
  <https://learn.microsoft.com/en-us/azure/container-apps/manage-secrets>.
- **Multi-tenancy.** One Container App, one admin token, one share.
- **Cloud Armor / WAF.** App Gateway can be upgraded to `WAF_v2`
  for OWASP rule coverage.

## Cost estimate (rough, West Europe, May 2026)

App Gateway Standard_v2 has a fixed-instance cost of **~$185/mo** at
capacity 1 (the dominant line item — there is no cheaper SKU that
supports managed TLS). Container Apps with `min=max=1` at
0.5 vCPU / 1 GiB billed continuously runs **~$36/mo**. Azure Files
LRS at 10 GiB Standard is **~$0.60/mo** plus a few cents for
transactions. Log Analytics ingestion at demo log volume is **~$2/mo**.
Public IP Standard is **~$3.50/mo**. **Expect ~$225–230/mo all-in.**
The App Gateway is the obvious cost driver — if you can live without
it and front Container Apps directly with its built-in custom domain
+ managed cert, total drops to **~$45/mo**.

## Teardown

```bash
terraform destroy \
  -var "admin_token=ignored"
```

Resource-group deletion under the hood, so everything goes together.
Allow ~5 min.
