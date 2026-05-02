# Packer template — bakes Docker, the latency_e2e docker-compose, and all five
# pre-pulled container images into an Amazon Linux 2023 AMI. Built so a Spot
# reclaim recovers in ~60-90s (no apt installs, no image pulls).
#
# Build manually:
#   cd wingfoil/examples/latency_e2e/pulumi/ec2-spot/packer
#   packer init wingfoil-latency.pkr.hcl
#   packer build \
#     -var ws_server_image=<registry>/wingfoil/ws-server:<tag> \
#     -var fix_gw_image=<registry>/wingfoil/fix-gw:<tag> \
#     -var prometheus_image=<registry>/wingfoil/prometheus:<tag> \
#     -var tempo_image=<registry>/wingfoil/tempo:<tag> \
#     -var grafana_image=<registry>/wingfoil/grafana:<tag> \
#     wingfoil-latency.pkr.hcl
#
# Output: an AMI named `wingfoil-latency-ec2-spot-<timestamp>` in the target
# region, tagged `Project=wingfoil-latency-ec2-spot`. The build's manifest
# (`packer-manifest.json`) records the AMI ID for downstream pipelines.

packer {
  required_plugins {
    amazon = {
      source  = "github.com/hashicorp/amazon"
      version = ">= 1.3.0"
    }
  }
}

variable "region" {
  type    = string
  default = "eu-west-2"
}

variable "instance_type" {
  type    = string
  default = "t3.small"
}

variable "ws_server_image"  { type = string }
variable "fix_gw_image"     { type = string }
variable "prometheus_image" { type = string }
variable "tempo_image"      { type = string }
variable "grafana_image"    { type = string }

variable "image_tag" {
  type        = string
  default     = "latest"
  description = "Tag suffix on the resulting AMI name (defaults to a timestamp)."
}

# ECR registry that the build host should `docker login` to before pulling the
# five wingfoil images. Empty string = anonymous Docker Hub pulls.
variable "ecr_registry" {
  type    = string
  default = ""
}

locals {
  ami_name = "wingfoil-latency-ec2-spot-${var.image_tag}-${formatdate("YYYYMMDD-hhmmss", timestamp())}"
}

source "amazon-ebs" "wingfoil" {
  region        = var.region
  instance_type = var.instance_type
  ami_name      = local.ami_name
  ssh_username  = "ec2-user"

  # Amazon Linux 2023 minimal — small, fast boot, dnf-based.
  source_ami_filter {
    filters = {
      name                = "al2023-ami-2023.*-x86_64"
      virtualization-type = "hvm"
      root-device-type    = "ebs"
      architecture        = "x86_64"
    }
    owners      = ["137112412989"] # Amazon
    most_recent = true
  }

  launch_block_device_mappings {
    device_name           = "/dev/xvda"
    volume_size           = 20
    volume_type           = "gp3"
    delete_on_termination = true
  }

  tags = {
    Name      = local.ami_name
    Project   = "wingfoil-latency-ec2-spot"
    ManagedBy = "Packer"
  }
}

build {
  name    = "wingfoil-latency"
  sources = ["source.amazon-ebs.wingfoil"]

  # Provision Docker, AWS CLI, the spot watcher, the docker-compose file, and
  # pre-pull all five service images so first boot is purely "compose up".
  provisioner "file" {
    source      = "${path.root}/../spot_watcher.py"
    destination = "/tmp/spot_watcher.py"
  }

  provisioner "file" {
    source      = "${path.root}/../../../docker-compose.yml"
    destination = "/tmp/docker-compose.yml"
  }

  provisioner "shell" {
    environment_vars = [
      "WS_SERVER_IMAGE=${var.ws_server_image}",
      "FIX_GW_IMAGE=${var.fix_gw_image}",
      "PROMETHEUS_IMAGE=${var.prometheus_image}",
      "TEMPO_IMAGE=${var.tempo_image}",
      "GRAFANA_IMAGE=${var.grafana_image}",
      "ECR_REGISTRY=${var.ecr_registry}",
      "AWS_REGION=${var.region}",
    ]
    script = "${path.root}/install.sh"
  }

  post-processor "manifest" {
    output     = "packer-manifest.json"
    strip_path = true
  }
}
