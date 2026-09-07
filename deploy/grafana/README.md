# Syntra Grafana Dashboard and Prometheus Alert Rules

This directory contains:

- `dashboards/syntra-overview.json` — Grafana 10.x dashboard (schemaVersion 39)
- `alerts/syntra-alerts.yaml` — Prometheus alerting rules

For what to do when an alert fires, see [`docs/runbook.md`](../../docs/runbook.md),
specifically Section 3 (Monitoring playbook) and the worked failure scenarios in
Section 2.

---

## Datasource assumption

All panels reference the datasource variable `${DS_PROMETHEUS}`. This is a
Grafana input variable that resolves to a Prometheus datasource at import time.
When you import via the Grafana UI you will be prompted to select which
Prometheus datasource to bind to. When provisioning via `dashboards.yaml` (see
below), set the `datasource` key in the provisioning config.

The Prometheus job that scrapes Syntra must be named `syntra` for the
`SyntraServerDown` alert (`up{job="syntra"} == 0`) to fire correctly. If your
job has a different name, update the `expr` in `alerts/syntra-alerts.yaml`
accordingly.

Syntra's `/metrics` endpoint requires a Bearer token. Add the admin key to
your Prometheus scrape config:

```yaml
scrape_configs:
  - job_name: syntra
    static_configs:
      - targets: ["syntra-host:8787"]
    authorization:
      credentials: "<LYCAN_ADMIN_KEY value>"
```

---

## Importing the dashboard

### Option A — Grafana UI

1. Open Grafana and navigate to **Dashboards > Import**.
2. Click **Upload JSON file** and select
   `deploy/grafana/dashboards/syntra-overview.json`, or paste the file
   contents into the text area.
3. When prompted, select the Prometheus datasource to bind to
   `${DS_PROMETHEUS}`.
4. Click **Import**.

### Option B — Grafana provisioning (recommended for production)

Place the dashboard JSON in a directory that Grafana's dashboard provisioner
watches, then add or extend your `dashboards.yaml` provisioning config:

```yaml
# /etc/grafana/provisioning/dashboards/syntra.yaml
apiVersion: 1

providers:
  - name: syntra
    orgId: 1
    type: file
    disableDeletion: false
    updateIntervalSeconds: 60
    allowUiUpdates: false
    options:
      path: /var/lib/grafana/dashboards/syntra
      foldersFromFilesStructure: false
```

Copy the dashboard file to the watched path:

```bash
mkdir -p /var/lib/grafana/dashboards/syntra
cp deploy/grafana/dashboards/syntra-overview.json \
   /var/lib/grafana/dashboards/syntra/
```

Grafana will pick up the dashboard at the next provisioning interval (or
immediately after a reload). The datasource UID must match a datasource
already provisioned in Grafana; update the `uid` field in
`__inputs[0]` if your Prometheus datasource has a different UID.

---

## Loading the alert rules

### Option A — Prometheus rule_files (bare-metal or docker-compose)

Add the rules file path to your `prometheus.yml`:

```yaml
rule_files:
  - /etc/prometheus/rules/syntra-alerts.yaml
```

Copy the file and reload Prometheus:

```bash
cp deploy/grafana/alerts/syntra-alerts.yaml /etc/prometheus/rules/
curl -X POST http://localhost:9090/-/reload
```

Validate the rules before loading:

```bash
promtool check rules deploy/grafana/alerts/syntra-alerts.yaml
```

### Option B — Kubernetes ConfigMap

```bash
kubectl create configmap syntra-alert-rules \
  --from-file=syntra-alerts.yaml=deploy/grafana/alerts/syntra-alerts.yaml \
  --namespace monitoring \
  --dry-run=client -o yaml | kubectl apply -f -
```

Then reference the ConfigMap in your Prometheus operator `PrometheusRule`
resource or in your Prometheus Helm values under `serverFiles.alerting_rules`.

If you are using the Prometheus Operator (kube-prometheus-stack), create a
`PrometheusRule` CRD instead:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PrometheusRule
metadata:
  name: syntra-alerts
  namespace: monitoring
  labels:
    release: kube-prometheus-stack   # must match the Prometheus operator selector
spec:
  groups:
    # paste the contents of alerts/syntra-alerts.yaml groups: block here
```

---

## Alert reference

| Alert | Severity | Condition | Runbook section |
|---|---|---|---|
| SyntraHighDecideLatency | warning | p99 > 100 ms for 5 min | Scenario 6 |
| SyntraHighRefusalRate | critical | refusals > 50% of decides for 5 min | Scenario 1 |
| SyntraCapsuleStuckInWarmup | info | warmup_state == 0 for 1 h | Scenario 2 |
| SyntraServerDown | critical | up{job="syntra"} == 0 for 1 min | Section 1 triage / §4.3 |
| SyntraDecideErrorRate | warning | error rate > 5% for 5 min | Section 1 triage |
| SyntraCapsuleFrozenLearning | info | warmup_state == 2 for 4 h | Scenario 4 |
| SyntraMetaBanditExplorationCollapsed | info | one candidate > 95% of trials for 30 min | Scenario 3 |
| SyntraStorageHigh | warning | store volume > 80% for 15 min | Scenario 5 |

`SyntraStorageHigh` requires Node Exporter metrics from the host that holds
the Syntra store volume (`/var/lib/syntra`). It will be a no-op if those
metrics are not present.

See [`docs/runbook.md`](../../docs/runbook.md) for full diagnostic queries and
remediation steps for each alert condition.
