"""
Always-on EC2 Spot deployment of the wingfoil latency_e2e demo.

Sibling stack to `fargate/` and `baremetal/`. Trades Fargate's auto-restart
convenience for ~3-4x cheaper compute by running on a single t3.small Spot
instance with a Packer-baked AMI. Single-AZ ASG of size 1 in eu-west-2a
(London, near LMAX LD4); reclaims trigger an automatic relaunch and a
~60-90s downtime gap during which the Grafana banner shows the spot
interruption countdown.

What this stack provisions:

  * VPC + single public subnet pinned to one AZ (default eu-west-2a)
  * Security group: 8080 (WS server), 9091 (prometheus), 3000 (grafana)
  * Pre-allocated Elastic IP — re-attached on every boot via user_data
  * Launch template + ASG (min/max/desired = 1) requesting Spot capacity,
    capped at on-demand price so we never pay more than the on-demand rate
  * Secrets Manager: LMAX FIX username + password
  * IAM instance profile: GetSecretValue + AssociateAddress on this EIP

Prerequisites:

  - The five latency_e2e Docker images pushed to a registry (ECR or Hub)
  - A Packer-built AMI ID; build it locally or via the GitHub Action at
    `.github/workflows/build-latency-e2e-ami.yml`
  - LMAX FIX credentials configured as Pulumi secrets:
      pulumi config set --secret lmax_username <...>
      pulumi config set --secret lmax_password <...>
  - The baked AMI ID:
      pulumi config set ami_id ami-0123456789abcdef0
"""

import json
from pathlib import Path

import pulumi
import pulumi_aws as aws

# ── Paths ────────────────────────────────────────────────────────────────
HERE = Path(__file__).resolve().parent

# ── Config ───────────────────────────────────────────────────────────────
config = pulumi.Config()
project = pulumi.get_project()
stack = pulumi.get_stack()
prefix = f"{project}-{stack}"

aws_region = aws.config.region or "eu-west-2"
availability_zone = config.get("availability_zone") or f"{aws_region}a"
instance_type = config.get("instance_type") or "t3.small"
ami_id = config.require("ami_id")
lmax_username = config.require_secret("lmax_username")
lmax_password = config.require_secret("lmax_password")
ingress_cidr = config.get("ingress_cidr") or "0.0.0.0/0"
# Cap at on-demand t3.small ($0.0228/hr in eu-west-2). With no cap, AWS
# defaults to on-demand price too — we set it explicitly to make the budget
# guarantee visible in the Pulumi diff.
max_spot_price = config.get("max_spot_price") or "0.0228"

tags = {"Project": project, "Stack": stack, "ManagedBy": "Pulumi"}

# ── Networking ───────────────────────────────────────────────────────────
# Single AZ: keeps EIP/EBS reattach simple, and a one-task demo doesn't
# benefit from multi-AZ failover (the ASG will relaunch in the same AZ).
vpc = aws.ec2.Vpc(
    f"{prefix}-vpc",
    cidr_block="10.0.0.0/16",
    enable_dns_hostnames=True,
    enable_dns_support=True,
    tags={**tags, "Name": f"{prefix}-vpc"},
)

subnet = aws.ec2.Subnet(
    f"{prefix}-subnet",
    vpc_id=vpc.id,
    cidr_block="10.0.1.0/24",
    availability_zone=availability_zone,
    map_public_ip_on_launch=True,
    tags={**tags, "Name": f"{prefix}-subnet"},
)

igw = aws.ec2.InternetGateway(
    f"{prefix}-igw",
    vpc_id=vpc.id,
    tags={**tags, "Name": f"{prefix}-igw"},
)

rt = aws.ec2.RouteTable(
    f"{prefix}-rt",
    vpc_id=vpc.id,
    routes=[aws.ec2.RouteTableRouteArgs(cidr_block="0.0.0.0/0", gateway_id=igw.id)],
    tags={**tags, "Name": f"{prefix}-rt"},
)
aws.ec2.RouteTableAssociation(
    f"{prefix}-rt-assoc",
    subnet_id=subnet.id,
    route_table_id=rt.id,
)

# Inline ingress (rather than standalone aws.vpc.SecurityGroupIngressRule
# resources) for two reasons:
#
#   1. The original SG was deployed with these exact inline rules, so the
#      live AWS state already contains them. Splitting them out into
#      standalone resources (PR #307) without first setting `ingress=[]`
#      to revoke the inline ones is a no-op as far as pulumi-aws is
#      concerned — the SG resource simply stops managing inline rules
#      and leaves the existing AWS rules in place — but the new
#      standalone SecurityGroupIngressRule resources then fail to
#      authorize with InvalidPermission.Duplicate (CI runs 25286544998,
#      25286756055).
#   2. The original concern behind the split — that inline rule edits
#      would replace the SG and cascade-replace the launch template — is
#      not actually how the AWS provider behaves. `ingress`/`egress` are
#      in-place updates via Authorize/RevokeSecurityGroupIngress; only
#      `description`, `name`, `name_prefix`, and `vpc_id` force
#      replacement. The catastrophic SG replacement seen in CI run
#      25286033743 was a description change, now neutralised by the
#      `ignore_changes=["description"]` below.
#
# Egress is intentionally not declared: AWS auto-creates a "0.0.0.0/0 allow
# all" egress rule on every new SG, and pulumi-aws leaves it alone unless
# an inline `egress` block is specified. Re-declaring it (inline or via
# aws.vpc.SecurityGroupEgressRule) fails with InvalidPermission.Duplicate
# (CI run 25286544998).
#
# 8080: ws_server HTTPS/WSS (terminated in-process via the `web-tls`
# feature); 3000: Grafana HTTPS. 9090 (Prometheus UI) and 9091
# (ws_server /metrics) stay plain-HTTP and are no longer exposed publicly —
# Prometheus scrapes via localhost on the host network. Operators who want
# the Prometheus UI can still reach it via an SSM session manager
# port-forward.
# If the SG is ever replaced (only via fields not already neutralised above —
# i.e. nothing we'd plausibly edit), the launch template flips to the new SG
# and the OLD SG can't be deleted until the running spot instance's ENI
# releases it. The terraform-aws-provider hardcodes a 15-min retry on
# DependencyViolation for SG delete (CI runs 25287037911 and 25288417368 both
# hung for 900s and gave up), and Pulumi's `custom_timeouts.delete` is
# engine-level only — it does not propagate to the bridged provider's
# `d.Timeout(schema.TimeoutDelete)`, so bumping it doesn't help. The deploy
# workflow handles this by draining the ASG to 0 *before* `pulumi up`, which
# guarantees no ENI references the old SG when Pulumi deletes it.
sg = aws.ec2.SecurityGroup(
    f"{prefix}-sg",
    vpc_id=vpc.id,
    description="wingfoil ec2-spot demo",
    ingress=[
        aws.ec2.SecurityGroupIngressArgs(
            from_port=8080, to_port=8080, protocol="tcp", cidr_blocks=[ingress_cidr]
        ),
        aws.ec2.SecurityGroupIngressArgs(
            from_port=3000, to_port=3000, protocol="tcp", cidr_blocks=[ingress_cidr]
        ),
    ],
    tags={**tags, "Name": f"{prefix}-sg"},
    opts=pulumi.ResourceOptions(ignore_changes=["description"]),
)

# ── Elastic IP ───────────────────────────────────────────────────────────
# Pre-allocated and *not* attached to a specific instance. user_data on the
# Spot instance associates it on every boot via the IAM permission below.
eip = aws.ec2.Eip(
    f"{prefix}-eip",
    domain="vpc",
    tags={**tags, "Name": f"{prefix}-eip"},
)

# ── Secrets ──────────────────────────────────────────────────────────────
lmax_username_secret = aws.secretsmanager.Secret(f"{prefix}-lmax-username", tags=tags)
aws.secretsmanager.SecretVersion(
    f"{prefix}-lmax-username-v",
    secret_id=lmax_username_secret.id,
    secret_string=lmax_username,
)
lmax_password_secret = aws.secretsmanager.Secret(f"{prefix}-lmax-password", tags=tags)
aws.secretsmanager.SecretVersion(
    f"{prefix}-lmax-password-v",
    secret_id=lmax_password_secret.id,
    secret_string=lmax_password,
)

# ── IAM: instance profile ────────────────────────────────────────────────
instance_role = aws.iam.Role(
    f"{prefix}-instance-role",
    assume_role_policy=json.dumps({
        "Version": "2012-10-17",
        "Statement": [{
            "Action": "sts:AssumeRole",
            "Effect": "Allow",
            "Principal": {"Service": "ec2.amazonaws.com"},
        }],
    }),
    tags=tags,
)

account_id = aws.get_caller_identity().account_id

aws.iam.RolePolicy(
    f"{prefix}-instance-policy",
    role=instance_role.id,
    policy=pulumi.Output.all(
        lmax_username_secret.arn,
        lmax_password_secret.arn,
        eip.allocation_id,
    ).apply(lambda a: json.dumps({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["secretsmanager:GetSecretValue"],
                "Resource": [a[0], a[1]],
            },
            # DescribeAddresses doesn't support resource-level permissions
            # at all (the previous tag condition was silently a no-op), so
            # split it out as `Resource: "*"` and accept the breadth — it's
            # a read-only API.
            {
                "Effect": "Allow",
                "Action": ["ec2:DescribeAddresses"],
                "Resource": "*",
            },
            # AssociateAddress: scope to *this* EIP allocation ARN plus any
            # instance/network-interface tagged with our Project/Stack. The
            # resource-tag condition is enforced per-resource — both the
            # EIP and the instance must satisfy it for the call to succeed.
            {
                "Effect": "Allow",
                "Action": ["ec2:AssociateAddress"],
                "Resource": [
                    f"arn:aws:ec2:*:{account_id}:elastic-ip/{a[2]}",
                    f"arn:aws:ec2:*:{account_id}:instance/*",
                    f"arn:aws:ec2:*:{account_id}:network-interface/*",
                ],
                "Condition": {
                    "StringEquals": {
                        "aws:ResourceTag/Project": project,
                        "aws:ResourceTag/Stack": stack,
                    }
                },
            },
        ],
    })),
)

# Attach AWS-managed `AmazonSSMManagedInstanceCore` so the SSM agent (baked
# into AL2023 AMIs by default) can register with Systems Manager. Without
# this, `aws ssm start-session` and the EC2 console "Connect → Session
# Manager" button both fail with AccessDeniedException — visible in the
# system log as
#     "SSM Agent unable to acquire credentials: ... AccessDeniedException:
#      Systems Manager's instance management role is not configured for
#      account: <id>"
# That makes runtime debugging impossible (no SSH port open either), so
# every container-runtime failure has to be diagnosed from boot logs alone.
# This managed policy is the standard SSM bootstrap and grants only what
# the agent needs (ssmmessages:*, ec2messages:*, ssm:Update*/Get*/Put*).
aws.iam.RolePolicyAttachment(
    f"{prefix}-instance-ssm-attach",
    role=instance_role.name,
    policy_arn="arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore",
)

instance_profile = aws.iam.InstanceProfile(
    f"{prefix}-instance-profile",
    role=instance_role.name,
)

# ── Launch template + ASG ────────────────────────────────────────────────
user_data_template = (HERE / "user_data.sh").read_text()


def render_user_data(args):
    eip_alloc, user_arn, pw_arn = args
    rendered = (
        user_data_template
        .replace("__EIP_ALLOCATION_ID__",     eip_alloc)
        .replace("__LMAX_USERNAME_SECRET__",  user_arn)
        .replace("__LMAX_PASSWORD_SECRET__",  pw_arn)
        .replace("__AWS_REGION__",            aws_region)
    )
    # cloud-init needs base64 user_data when passed via launch templates.
    import base64
    return base64.b64encode(rendered.encode("utf-8")).decode("ascii")


user_data_b64 = pulumi.Output.all(
    eip.allocation_id, lmax_username_secret.arn, lmax_password_secret.arn
).apply(render_user_data)

launch_template = aws.ec2.LaunchTemplate(
    f"{prefix}-lt",
    image_id=ami_id,
    instance_type=instance_type,
    iam_instance_profile=aws.ec2.LaunchTemplateIamInstanceProfileArgs(
        name=instance_profile.name,
    ),
    network_interfaces=[
        aws.ec2.LaunchTemplateNetworkInterfaceArgs(
            associate_public_ip_address="true",
            security_groups=[sg.id],
        ),
    ],
    instance_market_options=aws.ec2.LaunchTemplateInstanceMarketOptionsArgs(
        market_type="spot",
        spot_options=aws.ec2.LaunchTemplateInstanceMarketOptionsSpotOptionsArgs(
            max_price=max_spot_price,
            spot_instance_type="one-time",
            instance_interruption_behavior="terminate",
        ),
    ),
    metadata_options=aws.ec2.LaunchTemplateMetadataOptionsArgs(
        http_tokens="required",
        http_endpoint="enabled",
    ),
    user_data=user_data_b64,
    tag_specifications=[
        aws.ec2.LaunchTemplateTagSpecificationArgs(
            resource_type="instance",
            tags={**tags, "Name": f"{prefix}-host"},
        ),
        aws.ec2.LaunchTemplateTagSpecificationArgs(
            resource_type="volume",
            tags=tags,
        ),
    ],
    tags=tags,
)

# Single-AZ ASG of exactly one instance. ASG handles the relaunch when Spot
# reclaims the box (~2 min warning then terminate); the new instance runs
# user_data which reassociates the EIP.
asg = aws.autoscaling.Group(
    f"{prefix}-asg",
    vpc_zone_identifiers=[subnet.id],
    min_size=1,
    max_size=1,
    desired_capacity=1,
    health_check_type="EC2",
    health_check_grace_period=120,
    launch_template=aws.autoscaling.GroupLaunchTemplateArgs(
        id=launch_template.id,
        version="$Latest",
    ),
    # Roll the instance whenever the launch template changes (e.g. new AMI).
    instance_refresh=aws.autoscaling.GroupInstanceRefreshArgs(
        strategy="Rolling",
        preferences=aws.autoscaling.GroupInstanceRefreshPreferencesArgs(
            min_healthy_percentage=0,  # only ever 1 instance — drop it before launching the new one
        ),
    ),
    tags=[
        aws.autoscaling.GroupTagArgs(key="Project",   value=project,  propagate_at_launch=True),
        aws.autoscaling.GroupTagArgs(key="Stack",     value=stack,    propagate_at_launch=True),
        aws.autoscaling.GroupTagArgs(key="ManagedBy", value="Pulumi", propagate_at_launch=True),
        aws.autoscaling.GroupTagArgs(key="Name",      value=f"{prefix}-host", propagate_at_launch=True),
    ],
)

# ── Outputs ──────────────────────────────────────────────────────────────
pulumi.export("public_ip",      eip.public_ip)
# Both endpoints terminate TLS with a self-signed cert regenerated on
# every boot (see user_data.sh). Browsers will show a one-time warning
# until you accept the cert; the `wingfoil-js` client respects
# `location.protocol`, so the UI auto-upgrades to `wss://`.
pulumi.export("ws_server_url",  eip.public_ip.apply(lambda ip: f"https://{ip}:8080"))
pulumi.export("grafana_url",    eip.public_ip.apply(lambda ip: f"https://{ip}:3000"))
pulumi.export("asg_name",       asg.name)
pulumi.export("availability_zone", availability_zone)
