# Syntra — Terraform deployment modules

The modules in this directory deploy Syntra as a **single-container
serverless workload** on each of the three major clouds:

| Cloud | Service | Persistent storage | TLS frontend |
|-------|---------|--------------------|--------------|
| AWS   | ECS Fargate | EFS | ALB + ACM |
| GCP   | Cloud Run | Filestore | Cloud Run domain mapping (Google-managed cert) |
| Azure | Container Apps | Azure Files | Application Gateway |

Pick the cloud you already have an account on. Read that module's
`README.md` for prerequisites, `terraform apply` walkthrough, DNS
pointing instructions, and a rough monthly cost estimate.

## When to use these modules

- You want Syntra deployed in 10 minutes against a cloud account you
  already have, without standing up a Kubernetes cluster.
- You want the cloud to handle TLS, the load balancer, scaling, and
  the persistent volume — you write zero YAML and run zero
  `kubectl` commands.
- You're operating a small number of capsules and don't need
  multi-tenant isolation at the cluster level.

Rough cost expectations (all three modules document this in their own
README — see those for the line-item breakdown):

- AWS ≈ $30–35/month
- GCP ≈ $260/month (Filestore minimum-capacity floor dominates)
- Azure ≈ $225–230/month (App Gateway dominates; Container Apps
  built-in custom-domain route gets this to ~$45)

## When to use the Helm chart instead

If you're already running Kubernetes (EKS, GKE, AKS, on-prem, k3s,
anything), the Helm chart at
[`Syntra/deploy/helm/syntra/`](../helm/syntra/) is the better choice.
It deploys Syntra into your existing cluster as one workload among
many, uses your cluster's existing ingress controller for TLS, and
costs nothing beyond the cluster you're already paying for. The chart
README documents the configuration surface (auth tokens, persistent
volume sizing, Prometheus ServiceMonitor, optional HPA caveats).

## What's not covered

The same caveats apply to all three serverless modules; the per-cloud
READMEs repeat them with cloud-specific links:

- Multi-region / multi-AZ — modules deploy to a single region.
- Blue/green or canary releases — out of scope for a single-instance
  appliance.
- Secrets rotation — `admin_token` is passed in as a Terraform
  variable; rotate by re-applying with a new value.
- Cluster-style multi-tenancy — Syntra's single-binary model assumes
  one deployment per logical tenant boundary. For many tenants, run
  many deployments (or many Helm releases).

## Validation status

Each module's `terraform validate` passes against the current Terraform
binary (no `terraform apply` was run — that would incur real cloud
costs). See each module's README for the validation command.
