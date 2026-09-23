variable "region" {
  description = "AWS region. src/edit/gpu.rs defaults to the same one."
  type        = string
  default     = "us-east-1"
}

variable "profile" {
  description = "Local AWS profile (SSO). Run `aws sso login --profile dev` if credentials have expired."
  type        = string
  default     = "dev"
}

variable "name_prefix" {
  description = <<-EOT
    Prefix on every resource this stack owns, and the SSM path the app reads
    the stack's coordinates from (/<name_prefix>/…). src/edit/gpu.rs defaults
    to the same value; SAAGA_GPU_RENDER_STACK there selects another.
  EOT
  type        = string
  default     = "saaga-gpu-render"
}

variable "vpc_id" {
  description = "The VPC the render machines run in. The dev account has no default VPC."
  type        = string
  default     = "vpc-072524c6fef19b9d4"
}

variable "subnet_ids" {
  description = <<-EOT
    Public subnets to launch into, tried in order when one Availability Zone is
    out of GPU capacity. The machines get a public address for egress (npm, the
    Chrome download, S3) and accept no inbound traffic.
  EOT
  type = list(string)
  default = [
    "subnet-098ddb191ee6f09cc", # saaga-dev-public-us-east-1a
    "subnet-05f08c1e71f21f6a9", # saaga-dev-public-us-east-1b
    "subnet-0af77949f79f3022e", # saaga-dev-public-us-east-1c
  ]
}

variable "ami_id" {
  description = <<-EOT
    AWS Deep Learning Base OSS Nvidia Driver GPU AMI (Ubuntu 22.04): NVIDIA
    driver, EGL and the AWS CLI preinstalled. Pinned rather than resolved from
    /aws/service/deeplearning/… at launch, so a new AMI is a reviewed change
    and not a render that broke overnight. This is the image the 2026-09-23
    benchmark ran on.
  EOT
  type        = string
  default     = "ami-028e28e7fc9d87d00"
}

variable "instance_type" {
  description = <<-EOT
    NVIDIA T4, 8 vCPUs, 32 GB. Rendered a 3190-frame portrait chapter in 299s
    with 4 capture workers on 2026-09-23; software rendering on 16 vCPUs did
    480 frames in 45 minutes. The app may override it per run.
  EOT
  type        = string
  default     = "g4dn.2xlarge"
}

variable "volume_gb" {
  description = "Root volume. Extracted footage frames for two portrait chapters at once fit comfortably."
  type        = number
  default     = 100
}

variable "run_retention_days" {
  description = "Manifests, status files, logs and outputs under runs/ expire after this. The app downloads every output as it lands."
  type        = number
  default     = 7
}

variable "blob_retention_days" {
  description = <<-EOT
    Uploaded inputs under blobs/ are content-addressed, so a re-render that
    reuses footage uploads nothing again. They expire after this.
  EOT
  type        = number
  default     = 30
}
