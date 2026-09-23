output "bucket" {
  description = "Inputs (blobs/), runs (runs/<id>/) and the worker scripts (worker/)."
  value       = aws_s3_bucket.render.id
}

output "launch_template_id" {
  description = "What the app launches render machines from. It reads this from SSM, not from here."
  value       = aws_launch_template.render.id
}

output "running" {
  description = "Render machines up right now."
  value       = "AWS_PROFILE=${var.profile} aws ec2 describe-instances --filters Name=tag:Role,Values=${var.name_prefix} Name=instance-state-name,Values=pending,running --query 'Reservations[].Instances[].[InstanceId,InstanceType,LaunchTime,Tags[?Key==`RenderRun`].Value|[0]]' --output table"
}

output "stop_all" {
  description = "Terminate every render machine — the emergency brake."
  value       = "AWS_PROFILE=${var.profile} aws ec2 terminate-instances --instance-ids $(AWS_PROFILE=${var.profile} aws ec2 describe-instances --filters Name=tag:Role,Values=${var.name_prefix} Name=instance-state-name,Values=pending,running --query 'Reservations[].Instances[].InstanceId' --output text)"
}
