# Render on AWS: GPU machines started for a render and gone after it.
#
# The app (src/edit/gpu.rs) does the orchestration; this stack only provides
# what it launches from:
#
#   Render pressed ──► inputs to blobs/, manifest to runs/<id>/
#                 ──► N × RunInstances from the launch template, one shard each
#   each machine  ──► worker/worker.py: install, fetch its jobs, render on the
#                     GPU, upload outputs + status, power off (= terminate)
#   the app       ──► polls runs/<id>/status/, downloads outputs as they land
#
# Why a GPU and not Lambda: HyperFrames' distributed Lambda path forces
# SwiftShader, and the talking-head compositions run ~5-10 s per frame in
# software — every chunk hit Lambda's 900 s ceiling. On a T4 the same chapter
# rendered whole in 299 s (benchmark of 2026-09-23).
#
# Nothing here runs or costs anything between renders except the bucket.

data "aws_caller_identity" "current" {}

locals {
  bucket = "${var.name_prefix}-${data.aws_caller_identity.current.account_id}"
}

# -----------------------------------------------------------------------------
# 1. The bucket
# -----------------------------------------------------------------------------
# Key layout:
#   blobs/<sha256>                   render inputs, content-addressed (expire)
#   runs/<id>/manifest.json          what to render, and which machine renders it
#   runs/<id>/status/<shard>.json    each machine's progress, rewritten every few s
#   runs/<id>/out/<job>.mp4          finished renders, downloaded by the app
#   runs/<id>/logs/<job>.log         hyperframes' own output, per job
#   worker/…                         the scripts every machine runs (this stack)
resource "aws_s3_bucket" "render" {
  bucket = local.bucket
}

resource "aws_s3_bucket_public_access_block" "render" {
  bucket = aws_s3_bucket.render.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "render" {
  bucket = aws_s3_bucket.render.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "render" {
  bucket = aws_s3_bucket.render.id

  rule {
    id     = "expire-runs"
    status = "Enabled"
    filter {
      prefix = "runs/"
    }
    expiration {
      days = var.run_retention_days
    }
  }

  rule {
    id     = "expire-blobs"
    status = "Enabled"
    filter {
      prefix = "blobs/"
    }
    expiration {
      days = var.blob_retention_days
    }
  }

  rule {
    id     = "expire-old-versions"
    status = "Enabled"
    filter {}
    noncurrent_version_expiration {
      noncurrent_days = 1
    }
  }

  rule {
    id     = "abort-incomplete-uploads"
    status = "Enabled"
    filter {}
    abort_incomplete_multipart_upload {
      days_after_initiation = 2
    }
  }
}

# Versioned, so an object overwritten by mistake — or by a machine that should
# not have — can be put back. Old versions go after a day; nothing here is
# precious for longer than a render.
resource "aws_s3_bucket_versioning" "render" {
  bucket = aws_s3_bucket.render.id
  versioning_configuration {
    status = "Enabled"
  }
}

# TLS or nothing, for the team's Macs and the machines alike.
data "aws_iam_policy_document" "bucket" {
  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.render.arn,
      "${aws_s3_bucket.render.arn}/*",
    ]
    principals {
      type        = "*"
      identifiers = ["*"]
    }
    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "render" {
  bucket = aws_s3_bucket.render.id
  policy = data.aws_iam_policy_document.bucket.json
  # Applied after the public access block, which this policy does not loosen.
  depends_on = [aws_s3_bucket_public_access_block.render]
}

# What every machine fetches at boot: the worker scripts, and the renderer's
# package.json and lockfile, so a machine installs exactly what the Mac does.
# In the bucket rather than baked into the launch template, so changing the
# worker is `terraform apply` and nothing else — and read-only to the machines
# (see render_s3), so a machine cannot change what the next one runs.
locals {
  worker_files = merge(
    { for f in fileset("${path.module}/worker", "*") : f => "${path.module}/worker/${f}" },
    {
      "package.json"      = "${path.module}/../../renderer/package.json"
      "package-lock.json" = "${path.module}/../../renderer/package-lock.json"
    },
  )
}

resource "aws_s3_object" "worker" {
  for_each    = local.worker_files
  bucket      = aws_s3_bucket.render.id
  key         = "worker/${each.key}"
  source      = each.value
  source_hash = filemd5(each.value)
}

# -----------------------------------------------------------------------------
# 2. What a render machine may do
# -----------------------------------------------------------------------------
data "aws_iam_policy_document" "ec2_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "render" {
  name               = "${var.name_prefix}-machine"
  assume_role_policy = data.aws_iam_policy_document.ec2_assume.json
}

# Its own credentials rather than anything the app hands it, so a render
# outlives an `aws sso login` expiring on the Mac. Least privilege: a machine
# reads the worker, the inputs and the manifests, and writes only a run's
# status, outputs and logs. It cannot touch worker/ — which every later machine
# runs as root — nor blobs/, which other renders reuse.
data "aws_iam_policy_document" "render_s3" {
  statement {
    sid     = "Read"
    actions = ["s3:GetObject"]
    resources = [
      "${aws_s3_bucket.render.arn}/worker/*",
      "${aws_s3_bucket.render.arn}/blobs/*",
      "${aws_s3_bucket.render.arn}/runs/*",
    ]
  }
  statement {
    sid     = "WriteRunResults"
    actions = ["s3:PutObject", "s3:AbortMultipartUpload"]
    resources = [
      "${aws_s3_bucket.render.arn}/runs/*/status/*",
      "${aws_s3_bucket.render.arn}/runs/*/out/*",
      "${aws_s3_bucket.render.arn}/runs/*/logs/*",
    ]
  }
}

resource "aws_iam_role_policy" "render_s3" {
  name   = "render-bucket"
  role   = aws_iam_role.render.id
  policy = data.aws_iam_policy_document.render_s3.json
}

# Session Manager, so a machine that misbehaves can be looked at from the
# console without opening a port. Only the channel itself: AWS's managed
# AmazonSSMManagedInstanceCore also grants ssm:GetParameter(s) on every
# parameter in the account, which a render machine has no business reading.
data "aws_iam_policy_document" "session_manager" {
  statement {
    actions = [
      "ssm:UpdateInstanceInformation",
      "ssmmessages:CreateControlChannel",
      "ssmmessages:CreateDataChannel",
      "ssmmessages:OpenControlChannel",
      "ssmmessages:OpenDataChannel",
      "ec2messages:AcknowledgeMessage",
      "ec2messages:DeleteMessage",
      "ec2messages:FailMessage",
      "ec2messages:GetEndpoint",
      "ec2messages:GetMessages",
      "ec2messages:SendReply",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_role_policy" "session_manager" {
  name   = "session-manager"
  role   = aws_iam_role.render.id
  policy = data.aws_iam_policy_document.session_manager.json
}

resource "aws_iam_instance_profile" "render" {
  name = "${var.name_prefix}-machine"
  role = aws_iam_role.render.name
}

# -----------------------------------------------------------------------------
# 3. The machine
# -----------------------------------------------------------------------------
resource "aws_security_group" "render" {
  name        = "${var.name_prefix}-machine"
  description = "GPU render machines: no inbound, egress only"
  vpc_id      = var.vpc_id

  egress {
    description = "npm, the Chrome download and S3"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

# The subnet, the public address and the user data come from the app at
# launch (one shard per machine, subnet fallback on a capacity error); the
# rest is fixed here.
resource "aws_launch_template" "render" {
  name                                 = "${var.name_prefix}-machine"
  image_id                             = var.ami_id
  instance_type                        = var.instance_type
  instance_initiated_shutdown_behavior = "terminate"
  update_default_version               = true

  iam_instance_profile {
    arn = aws_iam_instance_profile.render.arn
  }

  metadata_options {
    http_tokens   = "required"
    http_endpoint = "enabled"
  }

  block_device_mappings {
    device_name = "/dev/sda1"
    ebs {
      volume_size           = var.volume_gb
      volume_type           = "gp3"
      throughput            = 250
      delete_on_termination = true
    }
  }

  tag_specifications {
    resource_type = "instance"
    tags = {
      Name = "${var.name_prefix}-machine"
      Role = var.name_prefix
    }
  }

  tag_specifications {
    resource_type = "volume"
    tags = {
      Role = var.name_prefix
    }
  }
}

# -----------------------------------------------------------------------------
# 4. Where the app finds all of it
# -----------------------------------------------------------------------------
resource "aws_ssm_parameter" "bucket" {
  name  = "/${var.name_prefix}/bucket"
  type  = "String"
  value = aws_s3_bucket.render.id
}

resource "aws_ssm_parameter" "launch_template" {
  name  = "/${var.name_prefix}/launch-template-id"
  type  = "String"
  value = aws_launch_template.render.id
}

resource "aws_ssm_parameter" "subnets" {
  name  = "/${var.name_prefix}/subnet-ids"
  type  = "StringList"
  value = join(",", var.subnet_ids)
}

resource "aws_ssm_parameter" "security_group" {
  name  = "/${var.name_prefix}/security-group-id"
  type  = "String"
  value = aws_security_group.render.id
}
