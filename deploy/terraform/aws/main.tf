// Syntra on AWS — single-instance ECS Fargate task behind an
// Application Load Balancer, with an EFS access point for the
// persistent store and ACM for TLS. Optimised for "working demo in
// 10 minutes", not for multi-AZ production HA.

provider "aws" {
  region = var.region
}

// ── Networking ─────────────────────────────────────────────────────
// We reuse the account's default VPC + default subnets. Saves the 60+
// resources a from-scratch VPC would add. Fine for a demo; swap in
// your own VPC for anything real.
data "aws_vpc" "default" {
  default = true
}

data "aws_subnets" "default" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.default.id]
  }
}

// ── Security groups ────────────────────────────────────────────────
// ALB SG: public 443 + 80 ingress (locked by var.allowed_cidrs).
resource "aws_security_group" "alb" {
  name        = "syntra-alb"
  description = "Ingress to the Syntra ALB."
  vpc_id      = data.aws_vpc.default.id

  ingress {
    description = "HTTPS from allowed CIDRs."
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = var.allowed_cidrs
  }

  ingress {
    description = "HTTP for the 80->443 redirect (or for plain HTTP if no domain is set)."
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = var.allowed_cidrs
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

// Task SG: only the ALB SG may reach the task on 8787. Egress open
// so the task can pull from ghcr.io and mount EFS.
resource "aws_security_group" "task" {
  name        = "syntra-task"
  description = "Ingress to the Syntra ECS task from the ALB only."
  vpc_id      = data.aws_vpc.default.id

  ingress {
    description     = "Syntra API from ALB."
    from_port       = 8787
    to_port         = 8787
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

// EFS SG: NFS (2049) from the task SG only.
resource "aws_security_group" "efs" {
  name        = "syntra-efs"
  description = "NFS access to the Syntra EFS volume from the task only."
  vpc_id      = data.aws_vpc.default.id

  ingress {
    description     = "NFS from task."
    from_port       = 2049
    to_port         = 2049
    protocol        = "tcp"
    security_groups = [aws_security_group.task.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

// ── Persistent store: EFS ──────────────────────────────────────────
// Elastic NFS volume. Cheaper than provisioning a fixed EBS disk when
// you don't know how much state Syntra will accumulate.
resource "aws_efs_file_system" "store" {
  creation_token = "syntra-store"
  encrypted      = true

  tags = {
    Name = "syntra-store"
  }
}

// One mount target per default subnet so the task can attach from
// whichever AZ it ends up scheduled in.
resource "aws_efs_mount_target" "store" {
  for_each        = toset(data.aws_subnets.default.ids)
  file_system_id  = aws_efs_file_system.store.id
  subnet_id       = each.value
  security_groups = [aws_security_group.efs.id]
}

// Access point pins UID/GID and the root path inside EFS so the
// container always sees /store mapped to a stable EFS subdirectory.
resource "aws_efs_access_point" "store" {
  file_system_id = aws_efs_file_system.store.id

  posix_user {
    uid = 0
    gid = 0
  }

  root_directory {
    path = "/store"
    creation_info {
      owner_uid   = 0
      owner_gid   = 0
      permissions = "0755"
    }
  }
}

// ── ACM certificate (DNS-validated) ────────────────────────────────
// Only created if a domain was supplied. Certs are free; validation
// CNAMEs are written into Route53 automatically.
data "aws_route53_zone" "this" {
  count        = var.domain_name != "" ? 1 : 0
  name         = var.route53_zone_name
  private_zone = false
}

resource "aws_acm_certificate" "this" {
  count             = var.domain_name != "" ? 1 : 0
  domain_name       = var.domain_name
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_route53_record" "cert_validation" {
  for_each = var.domain_name != "" ? {
    for dvo in aws_acm_certificate.this[0].domain_validation_options : dvo.domain_name => {
      name   = dvo.resource_record_name
      record = dvo.resource_record_value
      type   = dvo.resource_record_type
    }
  } : {}

  zone_id = data.aws_route53_zone.this[0].zone_id
  name    = each.value.name
  type    = each.value.type
  records = [each.value.record]
  ttl     = 60
}

resource "aws_acm_certificate_validation" "this" {
  count                   = var.domain_name != "" ? 1 : 0
  certificate_arn         = aws_acm_certificate.this[0].arn
  validation_record_fqdns = [for r in aws_route53_record.cert_validation : r.fqdn]
}

// ── ALB ────────────────────────────────────────────────────────────
// Layer-7 load balancer. Terminates TLS at the edge and forwards
// plaintext HTTP to the task on 8787.
resource "aws_lb" "this" {
  name               = "syntra-alb"
  load_balancer_type = "application"
  security_groups    = [aws_security_group.alb.id]
  subnets            = data.aws_subnets.default.ids
}

resource "aws_lb_target_group" "this" {
  name        = "syntra-tg"
  port        = 8787
  protocol    = "HTTP"
  target_type = "ip" // Fargate tasks register by IP, not instance id.
  vpc_id      = data.aws_vpc.default.id

  health_check {
    // Syntra's healthcheck endpoint. If it's renamed, change here.
    path                = "/healthz"
    matcher             = "200-399"
    interval            = 30
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

// 80 → 443 redirect when a cert exists; otherwise forward 80 straight
// to the task so the demo still works without a domain.
resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.this.arn
  port              = 80
  protocol          = "HTTP"

  dynamic "default_action" {
    for_each = var.domain_name != "" ? [1] : []
    content {
      type = "redirect"
      redirect {
        port        = "443"
        protocol    = "HTTPS"
        status_code = "HTTP_301"
      }
    }
  }

  dynamic "default_action" {
    for_each = var.domain_name == "" ? [1] : []
    content {
      type             = "forward"
      target_group_arn = aws_lb_target_group.this.arn
    }
  }
}

resource "aws_lb_listener" "https" {
  count             = var.domain_name != "" ? 1 : 0
  load_balancer_arn = aws_lb.this.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate_validation.this[0].certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.this.arn
  }
}

// Route53 A-alias from the supplied domain to the ALB.
resource "aws_route53_record" "alb" {
  count   = var.domain_name != "" ? 1 : 0
  zone_id = data.aws_route53_zone.this[0].zone_id
  name    = var.domain_name
  type    = "A"

  alias {
    name                   = aws_lb.this.dns_name
    zone_id                = aws_lb.this.zone_id
    evaluate_target_health = true
  }
}

// ── ECS cluster + task ─────────────────────────────────────────────
// One cluster, one service, one task. Fargate so we don't manage EC2.
resource "aws_ecs_cluster" "this" {
  name = "syntra"
}

// IAM role the ECS agent uses to pull the image + ship logs.
data "aws_iam_policy_document" "task_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "task_execution" {
  name               = "syntra-task-execution"
  assume_role_policy = data.aws_iam_policy_document.task_assume.json
}

resource "aws_iam_role_policy_attachment" "task_execution" {
  role       = aws_iam_role.task_execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

// CloudWatch log group for the task's stdout/stderr.
resource "aws_cloudwatch_log_group" "syntra" {
  name              = "/ecs/syntra"
  retention_in_days = 7
}

// Task definition: one container, EFS volume mounted at /store,
// SYNTRA_ADMIN_KEY injected from var.admin_token.
resource "aws_ecs_task_definition" "this" {
  family                   = "syntra"
  cpu                      = "512"
  memory                   = "1024"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  execution_role_arn       = aws_iam_role.task_execution.arn

  volume {
    name = "store"
    efs_volume_configuration {
      file_system_id     = aws_efs_file_system.store.id
      transit_encryption = "ENABLED"
      authorization_config {
        access_point_id = aws_efs_access_point.store.id
        iam             = "DISABLED"
      }
    }
  }

  container_definitions = jsonencode([
    {
      name      = "syntra"
      image     = "ghcr.io/ashhart/syntra:${var.image_tag}"
      essential = true

      portMappings = [
        {
          containerPort = 8787
          hostPort      = 8787
          protocol      = "tcp"
        }
      ]

      environment = [
        // Override the binary's default store root so it writes into
        // the EFS-backed mount instead of the image-local /syntra/data.
        { name = "LYCAN_STORE_ROOT", value = "/store" },
        { name = "SYNTRA_ADMIN_KEY", value = var.admin_token },
      ]

      mountPoints = [
        {
          sourceVolume  = "store"
          containerPath = "/store"
          readOnly      = false
        }
      ]

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = aws_cloudwatch_log_group.syntra.name
          awslogs-region        = var.region
          awslogs-stream-prefix = "syntra"
        }
      }
    }
  ])
}

// Service that keeps exactly one task running. desired_count = 1
// because Syntra is single-writer in this image.
resource "aws_ecs_service" "this" {
  name            = "syntra"
  cluster         = aws_ecs_cluster.this.id
  task_definition = aws_ecs_task_definition.this.arn
  desired_count   = 1
  launch_type     = "FARGATE"

  network_configuration {
    subnets          = data.aws_subnets.default.ids
    security_groups  = [aws_security_group.task.id]
    assign_public_ip = true // Needed in default VPC to pull from ghcr.io.
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.this.arn
    container_name   = "syntra"
    container_port   = 8787
  }

  depends_on = [
    aws_lb_listener.http,
    aws_efs_mount_target.store,
  ]
}
