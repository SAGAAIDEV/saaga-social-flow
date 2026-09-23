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
    id     = "abort-incomplete-uploads"
    status = "Enabled"
    filter {}
    abort_incomplete_multipart_upload {
      days_after_initiation = 2
    }
  }
}

# The scripts every machine fetches at boot. In the bucket rather than baked
# into the launch template, so changing the worker is `terraform apply` and
# nothing else.
resource "aws_s3_object" "worker" {
  for_each    = fileset("${path.module}/worker", "*")
  bucket      = aws_s3_bucket.render.id
  key         = "worker/${each.value}"
  source      = "${path.module}/worker/${each.value}"
  source_hash = filemd5("${path.module}/worker/${each.value}")
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

# Its own bucket and nothing else. Its own credentials rather than anything
# the app hands it, so a render outlives an `aws sso login` expiring on the Mac.
data "aws_iam_policy_document" "render_s3" {
  statement {
    actions   = ["s3:GetObject", "s3:PutObject", "s3:AbortMultipartUpload"]
    resources = ["${aws_s3_bucket.render.arn}/*"]
  }
  statement {
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.render.arn]
  }
}

resource "aws_iam_role_policy" "render_s3" {
  name   = "render-bucket"
  role   = aws_iam_role.render.id
  policy = data.aws_iam_policy_document.render_s3.json
}

# Session Manager, so a machine that misbehaves can be looked at from the
# console without opening a port.
resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.render.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
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
