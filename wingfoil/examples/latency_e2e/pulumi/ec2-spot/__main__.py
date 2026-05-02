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

sg = aws.ec2.SecurityGroup(
    f"{prefix}-sg",
    vpc_id=vpc.id,
    description="wingfoil ec2-spot demo — WS, prometheus, grafana",
    ingress=[
        aws.ec2.SecurityGroupIngressArgs(from_port=8080, to_port=8080, protocol="tcp", cidr_blocks=[ingress_cidr]),
        aws.ec2.SecurityGroupIngressArgs(from_port=9091, to_port=9091, protocol="tcp", cidr_blocks=[ingress_cidr]),
        aws.ec2.SecurityGroupIngressArgs(from_port=3000, to_port=3000, protocol="tcp", cidr_blocks=[ingress_cidr]),
    ],
    egress=[aws.ec2.SecurityGroupEgressArgs(from_port=0, to_port=0, protocol="-1", cidr_blocks=["0.0.0.0/0"])],
    tags={**tags, "Name": f"{prefix}-sg"},
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

aws.iam.RolePolicy(
    f"{prefix}-instance-policy",
    role=instance_role.id,
    policy=pulumi.Output.all(
        lmax_username_secret.arn, lmax_password_secret.arn, eip.allocation_id
    ).apply(lambda a: json.dumps({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["secretsmanager:GetSecretValue"],
                "Resource": [a[0], a[1]],
            },
            # AssociateAddress doesn't accept resource-level conditions on
            # the EIP itself; scope by tag on the instance instead so the
            # role can only re-associate to instances we own.
            {
                "Effect": "Allow",
                "Action": ["ec2:AssociateAddress", "ec2:DescribeAddresses"],
                "Resource": "*",
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
pulumi.export("ws_server_url",  eip.public_ip.apply(lambda ip: f"http://{ip}:8080"))
pulumi.export("grafana_url",    eip.public_ip.apply(lambda ip: f"http://{ip}:3000"))
pulumi.export("prometheus_url", eip.public_ip.apply(lambda ip: f"http://{ip}:9091/metrics"))
pulumi.export("asg_name",       asg.name)
pulumi.export("availability_zone", availability_zone)
