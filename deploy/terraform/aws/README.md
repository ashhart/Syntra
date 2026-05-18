# Syntra on AWS — Terraform module

Deploys a single-instance Syntra container as an ECS Fargate service
fronted by an Application Load Balancer with ACM TLS and an EFS
volume for persistent state.

## What it builds

- 1 ECS cluster, 1 service, 1 task running `ghcr.io/ashhart/syntra:<tag>`
- 1 EFS file system + access point mounted at `/store` inside the container
- 1 ALB on 443 (TLS) + 80 (redirect)
- 1 ACM certificate, DNS-validated via Route53 (only if `domain_name` is set)
- Security groups locking task→ALB and EFS→task ingress
- A CloudWatch log group at `/ecs/syntra` (7-day retention)

The module uses the account's **default VPC** and default subnets. If
your account has no default VPC, run `aws ec2 create-default-vpc` or
fork the module to point at your own.

The container's `LYCAN_STORE_ROOT` is overridden to `/store` so Syntra
writes its state into the EFS-backed mount rather than the image's
default `/syntra/data` directory.

## Prerequisites

- An AWS account with sufficient permissions: ECS, EC2, ELB, EFS,
  IAM, ACM, Route53, CloudWatch Logs (admin works; for least-privilege
  you can scope to those services).
- AWS CLI authenticated: `aws sts get-caller-identity` must succeed.
- Terraform >= 1.5.
- A Route53 public hosted zone for your domain (only if you want HTTPS).

## Quick start

```bash
cd Syntra/deploy/terraform/aws
terraform init
terraform plan \
  -var "admin_token=$(openssl rand -hex 32)" \
  -var "domain_name=syntra.example.com" \
  -var "route53_zone_name=example.com"
terraform apply \
  -var "admin_token=$(openssl rand -hex 32)" \
  -var "domain_name=syntra.example.com" \
  -var "route53_zone_name=example.com"
```

Apply takes ~5 minutes. ACM DNS validation is the slowest step
(2–3 min). The `endpoint_url` output is your public HTTPS URL.

For a no-TLS smoke test, omit `domain_name` and `route53_zone_name` —
you'll get a plain `http://...elb.amazonaws.com` URL.

## DNS

If `domain_name` is set and the zone lives in this account, the
module creates the A-alias automatically. If your zone is elsewhere,
leave `domain_name` blank, take the `alb_dns_name` output, and create
a CNAME yourself pointing at it.

## What's NOT covered

- **Multi-region / multi-AZ HA.** This is a single task in a single
  region. ECS will restart a dead task, but there is no warm standby.
  See <https://docs.aws.amazon.com/whitepapers/latest/aws-multi-region-fundamentals/>.
- **Blue/green deploys.** New images roll via ECS' default in-place
  strategy. For real cutover use CodeDeploy:
  <https://docs.aws.amazon.com/AmazonECS/latest/developerguide/deployment-type-bluegreen.html>.
- **Secrets rotation.** The admin token is baked into the task
  definition as a plain env var. For rotation, move it to AWS Secrets
  Manager and reference via `secrets[]` in the container definition:
  <https://docs.aws.amazon.com/AmazonECS/latest/developerguide/specifying-sensitive-data-secrets.html>.
- **Multi-tenancy.** One Syntra, one admin token, one EFS. Run the
  module again with a separate state file to add tenants.
- **Custom VPC / private subnets / NAT.** This uses the default VPC.
  Production should not.

## Cost estimate (rough, eu-west-1, May 2026)

A single Fargate task at 0.5 vCPU / 1 GB RAM runs roughly **$15/mo**
on-demand (24/7). The ALB is a flat **~$16/mo** plus a few cents of
LCU. EFS at the demo's expected footprint (<5 GB) is well under
**$1/mo**. A Route53 hosted zone (if you create one just for this) is
**$0.50/mo**. CloudWatch Logs at demo log volume is around **$0.50/mo**.
**Expect $32–35/mo all-in** with no traffic, under $40/mo at modest
demo traffic. ACM certs are free. The biggest swing variable is the
ALB — if you replace it with a Network Load Balancer or expose the
task directly via a public IP, you save ~$16/mo at the cost of losing
managed TLS and operational simplicity.

## Teardown

```bash
terraform destroy -var "admin_token=ignored"
```

EFS deletion is fast; ALB takes ~2 min to drain. Snapshot anything
you care about first.
