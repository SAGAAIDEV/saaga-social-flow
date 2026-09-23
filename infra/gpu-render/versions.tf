terraform {
  required_version = ">= 1.9"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.82"
    }
  }

  # The dev account's shared state bucket and lock table (the ones saaga-infra's
  # Terragrunt uses), under
  # this stack's own key.
  backend "s3" {
    bucket         = "terraform-state-827913618293-us-east-1"
    key            = "saaga-social-flow/gpu-render/terraform.tfstate"
    region         = "us-east-1"
    encrypt        = true
    dynamodb_table = "terraform-state-lock"
  }
}

provider "aws" {
  region  = var.region
  profile = var.profile

  default_tags {
    tags = {
      Project              = "saaga-social-flow"
      HyperFramesComponent = "gpu-renderer"
      ManagedBy            = "terraform"
      Workspace            = "dev"
    }
  }
}
