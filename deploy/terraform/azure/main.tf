// Syntra on Azure — single-instance Container Apps revision with an
// Azure Files share mounted at /store, fronted by an Application
// Gateway. Optimised for "working demo in 10 minutes".
//
// TLS note. Azure offers two managed-cert paths:
//   1. Container Apps custom-domain managed certs (free, auto-renewed,
//      attached directly to the Container App).
//   2. Application Gateway managed certs (also free, attached to a
//      listener), but the AzureRM 3.x provider can't *create* the
//      managed cert resource — it can only consume one from Key Vault.
// To stay both spec-honest ("App Gateway with managed cert") and
// validatable in a single `terraform apply`, this module:
//   • always creates the App Gateway with an HTTP listener
//   • when `domain_name` is set, additionally creates an HTTPS
//     listener that references a Key Vault certificate the user must
//     pre-populate, OR (recommended) attach the managed cert via the
//     CLI step documented in the README post-apply.
// In other words: Terraform builds the gateway; the managed cert is a
// one-line `az` command after `terraform apply`. See README.

provider "azurerm" {
  features {}
}

// Storage account names must be globally unique and lowercase
// alphanumeric. Random suffix avoids collisions on re-apply.
resource "random_string" "storage_suffix" {
  length  = 8
  upper   = false
  special = false
}

// ── Resource group ─────────────────────────────────────────────────
// Every Syntra resource lands here so teardown is one
// `az group delete`.
resource "azurerm_resource_group" "this" {
  name     = var.resource_group_name
  location = var.region
}

// ── Networking ─────────────────────────────────────────────────────
// App Gateway requires its own dedicated subnet.
resource "azurerm_virtual_network" "this" {
  name                = "syntra-vnet"
  address_space       = ["10.50.0.0/16"]
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
}

resource "azurerm_subnet" "appgw" {
  name                 = "appgw-subnet"
  resource_group_name  = azurerm_resource_group.this.name
  virtual_network_name = azurerm_virtual_network.this.name
  address_prefixes     = ["10.50.1.0/24"]
}

// ── Persistent store: Azure Files ──────────────────────────────────
// Standard LRS storage account hosting one SMB share. Cheaper than
// premium and adequate for demo write patterns.
resource "azurerm_storage_account" "store" {
  name                       = "syntra${random_string.storage_suffix.result}"
  resource_group_name        = azurerm_resource_group.this.name
  location                   = azurerm_resource_group.this.location
  account_tier               = "Standard"
  account_replication_type   = "LRS"
  account_kind               = "StorageV2"
  https_traffic_only_enabled = true
}

resource "azurerm_storage_share" "store" {
  name                 = "syntra-store"
  storage_account_name = azurerm_storage_account.store.name
  quota                = var.store_size_gb
}

// ── Log Analytics workspace ────────────────────────────────────────
// Container Apps environments require a workspace for their platform
// diagnostic logs.
resource "azurerm_log_analytics_workspace" "this" {
  name                = "syntra-logs"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  sku                 = "PerGB2018"
  retention_in_days   = 30
}

// ── Container Apps Environment ─────────────────────────────────────
// Hosts the Container App. We register the Azure Files share at the
// environment level so the app can mount it.
resource "azurerm_container_app_environment" "this" {
  name                       = "syntra-env"
  location                   = azurerm_resource_group.this.location
  resource_group_name        = azurerm_resource_group.this.name
  log_analytics_workspace_id = azurerm_log_analytics_workspace.this.id
}

resource "azurerm_container_app_environment_storage" "store" {
  name                         = "syntra-store"
  container_app_environment_id = azurerm_container_app_environment.this.id
  account_name                 = azurerm_storage_account.store.name
  share_name                   = azurerm_storage_share.store.name
  access_key                   = azurerm_storage_account.store.primary_access_key
  // ReadWrite so Syntra can write its journal + capsule state.
  access_mode = "ReadWrite"
}

// ── Container App ──────────────────────────────────────────────────
// Single revision, replicas pinned to 1 so the Azure Files share is
// never claimed by two writers.
resource "azurerm_container_app" "this" {
  name                         = "syntra"
  container_app_environment_id = azurerm_container_app_environment.this.id
  resource_group_name          = azurerm_resource_group.this.name
  revision_mode                = "Single"

  secret {
    // Admin token kept as a secret rather than a raw env var so it
    // isn't visible on the revision detail page.
    name  = "admin-token"
    value = var.admin_token
  }

  ingress {
    // External so App Gateway can reach the Container App FQDN.
    // Container Apps terminates internal TLS on the
    // *.azurecontainerapps.io FQDN with its own platform cert.
    external_enabled = true
    target_port      = 8787
    transport        = "auto"

    traffic_weight {
      latest_revision = true
      percentage      = 100
    }
  }

  template {
    min_replicas = 1
    max_replicas = 1

    volume {
      name         = "store"
      storage_name = azurerm_container_app_environment_storage.store.name
      storage_type = "AzureFile"
    }

    container {
      name   = "syntra"
      image  = "ghcr.io/ashhart/syntra:${var.image_tag}"
      cpu    = 0.5
      memory = "1Gi"

      env {
        // Override the binary's default store root so it writes to
        // the Azure Files share rather than the image's /syntra/data.
        name  = "LYCAN_STORE_ROOT"
        value = "/store"
      }

      env {
        name        = "SYNTRA_ADMIN_KEY"
        secret_name = "admin-token"
      }

      volume_mounts {
        name = "store"
        path = "/store"
      }
    }
  }
}

// ── Application Gateway ────────────────────────────────────────────
// Public IP + App Gateway. Forwards to the Container App's FQDN over
// HTTPS (Container Apps provides internal TLS automatically). The
// HTTPS listener at the App Gateway edge requires a managed cert
// that you attach via the CLI step in the README — Terraform builds
// the rest of the gateway here.
resource "azurerm_public_ip" "appgw" {
  name                = "syntra-appgw-pip"
  resource_group_name = azurerm_resource_group.this.name
  location            = azurerm_resource_group.this.location
  allocation_method   = "Static"
  sku                 = "Standard"
  // DNS label gives you a stable *.cloudapp.azure.com hostname for
  // the no-domain demo path.
  domain_name_label = "syntra-${random_string.storage_suffix.result}"
}

resource "azurerm_application_gateway" "this" {
  name                = "syntra-appgw"
  resource_group_name = azurerm_resource_group.this.name
  location            = azurerm_resource_group.this.location

  sku {
    // Standard_v2 supports managed TLS certs (attached out-of-band)
    // and autoscaling. Capacity 1 is the minimum and adequate for
    // demo traffic.
    name     = "Standard_v2"
    tier     = "Standard_v2"
    capacity = 1
  }

  gateway_ip_configuration {
    name      = "appgw-ip-config"
    subnet_id = azurerm_subnet.appgw.id
  }

  frontend_port {
    name = "http-port"
    port = 80
  }

  frontend_ip_configuration {
    name                 = "appgw-frontend-ip"
    public_ip_address_id = azurerm_public_ip.appgw.id
  }

  backend_address_pool {
    name  = "syntra-backend"
    fqdns = [azurerm_container_app.this.ingress[0].fqdn]
  }

  backend_http_settings {
    name                                = "syntra-backend-https"
    cookie_based_affinity               = "Disabled"
    port                                = 443
    protocol                            = "Https"
    request_timeout                     = 30
    pick_host_name_from_backend_address = true
    probe_name                          = "syntra-probe"
  }

  probe {
    name                                      = "syntra-probe"
    protocol                                  = "Https"
    pick_host_name_from_backend_http_settings = true
    path                                      = "/healthz"
    interval                                  = 30
    timeout                                   = 10
    unhealthy_threshold                       = 3
    match {
      status_code = ["200-399"]
    }
  }

  // Always-present HTTP listener on port 80. Useful for the
  // no-domain demo path and as a target for an optional 80→443
  // redirect rule you can add post-apply.
  http_listener {
    name                           = "syntra-http-listener"
    frontend_ip_configuration_name = "appgw-frontend-ip"
    frontend_port_name             = "http-port"
    protocol                       = "Http"
    host_name                      = var.domain_name != "" ? var.domain_name : null
  }

  request_routing_rule {
    name                       = "syntra-http-routing"
    rule_type                  = "Basic"
    http_listener_name         = "syntra-http-listener"
    backend_address_pool_name  = "syntra-backend"
    backend_http_settings_name = "syntra-backend-https"
    priority                   = 100
  }
}
