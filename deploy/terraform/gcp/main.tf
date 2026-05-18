// Syntra on GCP — Cloud Run service (single instance, min=max=1) with
// a Filestore NFS volume mounted at /store, fronted by a Google-managed
// TLS cert via Cloud Run domain mapping. Optimised for "working demo
// in 10 minutes".

provider "google" {
  project = var.project_id
  region  = var.region
}

// ── APIs ───────────────────────────────────────────────────────────
// Enable everything the module touches. Disabling on destroy is off so
// re-applies don't churn the project state.
resource "google_project_service" "run" {
  service            = "run.googleapis.com"
  disable_on_destroy = false
}

resource "google_project_service" "filestore" {
  service            = "file.googleapis.com"
  disable_on_destroy = false
}

resource "google_project_service" "compute" {
  // Required for VPC connector + default network.
  service            = "compute.googleapis.com"
  disable_on_destroy = false
}

resource "google_project_service" "vpcaccess" {
  service            = "vpcaccess.googleapis.com"
  disable_on_destroy = false
}

// ── Networking ─────────────────────────────────────────────────────
// Cloud Run reaches Filestore through a Serverless VPC Connector
// attached to the default VPC. The connector needs a /28 inside the
// VPC that doesn't collide with anything else.
data "google_compute_network" "default" {
  name       = "default"
  depends_on = [google_project_service.compute]
}

resource "google_vpc_access_connector" "this" {
  name          = "syntra-connector"
  region        = var.region
  network       = data.google_compute_network.default.name
  ip_cidr_range = "10.8.0.0/28"

  // Smallest sizing — single throughput min/max so we don't pay for
  // a fleet of connector instances during a demo.
  min_throughput = 200
  max_throughput = 300

  depends_on = [google_project_service.vpcaccess]
}

// ── Filestore ──────────────────────────────────────────────────────
// Persistent NFS store. BASIC_HDD is the cheapest tier; minimum
// provisioned capacity is 1024 GiB regardless of var.store_size_gb.
// We surface that in the README so nobody is surprised.
resource "google_filestore_instance" "store" {
  name     = "syntra-store"
  location = var.region
  tier     = "BASIC_HDD"

  file_shares {
    name        = "store"
    capacity_gb = max(1024, var.store_size_gb)
  }

  networks {
    network = data.google_compute_network.default.name
    modes   = ["MODE_IPV4"]
  }

  depends_on = [google_project_service.filestore]
}

// ── Cloud Run service ──────────────────────────────────────────────
// Single revision, min=max=1 because Syntra is single-writer in this
// image. The container has the Filestore share NFS-mounted at /store
// and exposes 8787. SYNTRA_ADMIN_KEY is injected from var.admin_token.
resource "google_cloud_run_v2_service" "this" {
  name     = "syntra"
  location = var.region

  // Public-facing by default — Cloud Run handles TLS termination on
  // its *.run.app endpoint with a Google-managed wildcard cert.
  ingress = "INGRESS_TRAFFIC_ALL"

  template {
    // Pin scaling to 1/1 so the NFS mount is never claimed by two
    // simultaneous revisions.
    scaling {
      min_instance_count = 1
      max_instance_count = 1
    }

    // Cloud Run gen2 execution environment is required for NFS mounts.
    execution_environment = "EXECUTION_ENVIRONMENT_GEN2"

    vpc_access {
      connector = google_vpc_access_connector.this.id
      egress    = "PRIVATE_RANGES_ONLY"
    }

    volumes {
      name = "store"
      nfs {
        server    = google_filestore_instance.store.networks[0].ip_addresses[0]
        path      = "/store"
        read_only = false
      }
    }

    containers {
      image = "ghcr.io/ashhart/syntra:${var.image_tag}"

      ports {
        // Cloud Run accepts one container port. 8787 matches Syntra's
        // API listener.
        container_port = 8787
      }

      env {
        // Override the binary's default store root so it writes into
        // the Filestore NFS mount.
        name  = "LYCAN_STORE_ROOT"
        value = "/store"
      }

      env {
        name  = "SYNTRA_ADMIN_KEY"
        value = var.admin_token
      }

      volume_mounts {
        name       = "store"
        mount_path = "/store"
      }

      resources {
        limits = {
          cpu    = "1"
          memory = "1Gi"
        }
        // Keep a CPU always allocated so background tasks inside
        // Syntra (e.g. scheduled flushes) keep ticking without
        // request-driven CPU.
        cpu_idle = false
      }
    }
  }

  depends_on = [google_project_service.run]
}

// Make the service publicly invokable. Without this, every request
// would need a Google identity token.
resource "google_cloud_run_v2_service_iam_member" "public" {
  project  = google_cloud_run_v2_service.this.project
  location = google_cloud_run_v2_service.this.location
  name     = google_cloud_run_v2_service.this.name
  role     = "roles/run.invoker"
  member   = "allUsers"
}

// ── Domain mapping + Google-managed cert ───────────────────────────
// Only created if a custom domain was supplied. The mapping provisions
// a Google-managed cert automatically — it usually takes 15–30 min
// after DNS resolves to become ACTIVE.
resource "google_cloud_run_domain_mapping" "this" {
  count    = var.domain_name != "" ? 1 : 0
  name     = var.domain_name
  location = var.region

  metadata {
    namespace = var.project_id
  }

  spec {
    route_name = google_cloud_run_v2_service.this.name
  }
}
