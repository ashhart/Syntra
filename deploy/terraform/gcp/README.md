# Syntra on GCP — Terraform module

Deploys a single-instance Syntra container on Cloud Run with a
Filestore NFS volume for persistent state and a Google-managed TLS
certificate provisioned via a Cloud Run domain mapping.

## What it builds

- 1 Cloud Run v2 service (`syntra`), `min=max=1` instance,
  `EXECUTION_ENVIRONMENT_GEN2` so NFS mounts work
- 1 Filestore instance (`BASIC_HDD`, NFS) mounted at `/store`
- 1 Serverless VPC Access connector in the default VPC so Cloud Run
  can reach Filestore
- 1 Cloud Run domain mapping (only if `domain_name` is set) which
  provisions a Google-managed TLS cert automatically
- A public IAM binding (`allUsers` → `run.invoker`) so the service
  is reachable from the internet

The container's `LYCAN_STORE_ROOT` is overridden to `/store` so
Syntra writes its state to the NFS share rather than the image's
default `/syntra/data` directory.

## Prerequisites

- A GCP project with billing enabled.
- `gcloud auth application-default login` set up (or service-account
  JSON via `GOOGLE_APPLICATION_CREDENTIALS`).
- The user / SA needs roles to enable APIs and create the resources:
  `roles/serviceusage.serviceUsageAdmin`, `roles/run.admin`,
  `roles/file.editor`, `roles/compute.networkAdmin`,
  `roles/vpcaccess.admin`, `roles/iam.serviceAccountUser`. `Owner`
  on the project also works.
- Terraform >= 1.5.

## Quick start

```bash
cd Syntra/deploy/terraform/gcp
terraform init
terraform plan \
  -var "project_id=my-gcp-project" \
  -var "admin_token=$(openssl rand -hex 32)" \
  -var "domain_name=syntra.example.com"
terraform apply \
  -var "project_id=my-gcp-project" \
  -var "admin_token=$(openssl rand -hex 32)" \
  -var "domain_name=syntra.example.com"
```

Apply takes ~12 minutes — Filestore takes 8–10 minutes to provision,
and a domain mapping's managed cert needs 15–30 min after DNS resolves
to become `ACTIVE`. The `endpoint_url` output is your public HTTPS URL.

Omit `domain_name` for the fastest path (~10 min). You'll get a
`https://syntra-<hash>-<region>.run.app` URL with TLS already done by
Google's wildcard cert.

## DNS

When you set `domain_name`, Cloud Run will not begin cert provisioning
until DNS resolves the name to the records it asks for. Read the
`domain_mapping_records` output and create those records (typically
4 × A records for the IPv4 endpoints, plus 4 × AAAA for IPv6) at your
DNS provider:

```bash
terraform output -json domain_mapping_records | jq
```

Once DNS is live, the managed cert auto-issues. Check status with:

```bash
gcloud beta run domain-mappings describe \
  --domain syntra.example.com --region <region>
```

## What's NOT covered

- **Multi-region.** Cloud Run is regional, Filestore is zonal/regional
  but anchored to one region here. See
  <https://cloud.google.com/run/docs/multiple-regions> for the
  multi-region load balancer pattern.
- **Blue/green.** New revisions get all traffic on apply. For gradual
  rollouts use Cloud Run traffic splitting:
  <https://cloud.google.com/run/docs/rollouts-rollbacks-traffic-migration>.
- **Secrets rotation.** Admin token is plain env on the revision.
  For rotation move it to Secret Manager and reference via `value_source`:
  <https://cloud.google.com/run/docs/configuring/secrets>.
- **Multi-tenancy.** One service, one admin token, one Filestore.
- **Private ingress / VPC SC.** Service is public. For internal-only
  set `ingress = "INGRESS_TRAFFIC_INTERNAL_ONLY"` and front with an
  internal LB.

## Cost estimate (rough, europe-west1, May 2026)

The big-ticket item is Filestore: `BASIC_HDD` minimum chargeable
capacity is **1024 GiB** even if you ask for less, at $0.20/GiB/mo →
**~$205/mo** flat. The Serverless VPC connector runs ~$10/mo at the
minimum throughput tier (200 Mbps). Cloud Run with `cpu_idle=false`
and `min=1` keeps 1 vCPU + 1 GiB always allocated, ~$45/mo. Cloud Run
egress to the internet is metered separately (free up to 1 GiB/mo,
then $0.12/GiB to most regions). Managed certs are free.
**Expect ~$260/mo all-in.** If Filestore cost is prohibitive, swap to
a smaller `ZONAL` tier in `main.tf` or back the store with GCS via the
GCS Fuse mount (different code path; not implemented here).

## Teardown

```bash
terraform destroy \
  -var "project_id=my-gcp-project" \
  -var "admin_token=ignored"
```

Filestore deletion takes ~5 min. APIs enabled by the module stay
enabled (`disable_on_destroy = false`) so re-applies don't churn state.
