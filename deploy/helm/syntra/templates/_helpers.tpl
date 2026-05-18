{{/*
Expand the name of the chart.
*/}}
{{- define "syntra.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Create a fully qualified app name. Truncated to 63 chars to stay within
K8s DNS label limits; trailing dashes are trimmed.
*/}}
{{- define "syntra.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Chart name and version label.
*/}}
{{- define "syntra.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels applied to every resource.
*/}}
{{- define "syntra.labels" -}}
helm.sh/chart: {{ include "syntra.chart" . }}
{{ include "syntra.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: syntra
{{- end -}}

{{/*
Selector labels — must remain stable across upgrades (a change here
breaks the Deployment's selector).
*/}}
{{- define "syntra.selectorLabels" -}}
app.kubernetes.io/name: {{ include "syntra.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
ServiceAccount name to use.
*/}}
{{- define "syntra.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "syntra.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
Resolve the name of the Secret carrying the admin token. Either the
user-supplied existingSecret or the chart-rendered one.
*/}}
{{- define "syntra.secretName" -}}
{{- if .Values.syntra.existingSecret -}}
{{- .Values.syntra.existingSecret -}}
{{- else -}}
{{- printf "%s-admin" (include "syntra.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/*
Resolve the name of the PVC carrying the store.
*/}}
{{- define "syntra.pvcName" -}}
{{- if .Values.persistence.existingClaim -}}
{{- .Values.persistence.existingClaim -}}
{{- else -}}
{{- printf "%s-store" (include "syntra.fullname" .) -}}
{{- end -}}
{{- end -}}
