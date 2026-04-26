"""
On-demand bare-metal deployment of the wingfoil latency_e2e demo.

Companion to the always-on `fargate/` stack. Spin up when you want to
showcase real per-hop perf numbers (sub-microsecond intra-process hops via
iceoryx2, isolated CPU cores, no hypervisor jitter); tear down (or just
`aws ec2 stop-instances`) when you're done.

What this stack provisions:

  * VPC + public subnet (one AZ — perf, not redundancy)
  * Security group: 8080 (WS server), 9091 (prometheus exporter),
    3000 (grafana), 22 (SSH) — all 0.0.0.0/0 by default for demo simplicity
  * One bare-metal EC2 instance (default c7i.metal-24xl)
  * Elastic IP — survives stop/start so the URL doesn't change
  * S3 bucket for binaries + observability configs + static web assets
  * Secrets Manager: LMAX FIX username + password
  * IAM instance profile granting read on the secrets and the bucket

You upload the release binaries to the bucket; cloud-init on the instance
syncs them down on first boot, configures grub for `isolcpus=`, reboots
once, then starts ws_server + fix_gw as systemd units pinned to the
isolated cores.

Prerequisites:

  cargo build --release -p wingfoil --example latency_e2e_ws_server \\
      --features "web,iceoryx2-beta,prometheus,otlp"
  cargo build --release -p wingfoil --example latency_e2e_fix_gw \\
      --features "fix,iceoryx2-beta"
  pulumi config set --secret lmax_username <...>
  pulumi config set --secret lmax_password <...>
"""

import json
import mimetypes
import os
from pathlib import Path

import pulumi
import pulumi_aws as aws

# ── Paths ────────────────────────────────────────────────────────────────
# Resolve everything relative to this file rather than CWD so
# `pulumi up` works regardless of where it's invoked.
HERE = Path(__file__).resolve().parent
EXAMPLE_DIR = HERE.parent.parent  # wingfoil/examples/latency_e2e
WORKSPACE_ROOT = HERE.parents[4]  # repo root
TARGET_RELEASE = WORKSPACE_ROOT / "target" / "release" / "examples"

WS_SERVER_BIN = TARGET_RELEASE / "latency_e2e_ws_server"
FIX_GW_BIN = TARGET_RELEASE / "latency_e2e_fix_gw"

for path, hint in [
    (WS_SERVER_BIN, "cargo build --release -p wingfoil --example latency_e2e_ws_server --features 'web,iceoryx2-beta,prometheus,otlp'"),
    (FIX_GW_BIN, "cargo build --release -p wingfoil --example latency_e2e_fix_gw --features 'fix,iceoryx2-beta'"),
]:
    if not path.exists():
        raise pulumi.RunError(
            f"required binary missing: {path}\n  build it with:  {hint}"
        )

# ── Config ───────────────────────────────────────────────────────────────
config = pulumi.Config()
project = pulumi.get_project()
stack = pulumi.get_stack()
prefix = f"{project}-{stack}"

aws_region = aws.config.region or "eu-west-2"
instance_type = config.get("instance_type") or "c7i.metal-24xl"
isolated_cores = config.get("isolated_cores") or "2,3,4,5"
ws_server_core = config.get("ws_server_core") or "2"
fix_gw_core = config.get("fix_gw_core") or "4"
lmax_username = config.require_secret("lmax_username")
lmax_password = config.require_secret("lmax_password")
alert_email = config.get("alert_email") or ""
monthly_budget_usd = config.get("monthly_budget_usd") or "50"

tags = {"Project": project, "Stack": stack, "ManagedBy": "Pulumi"}

# ── Networking ───────────────────────────────────────────────────────────
# Single public subnet — this stack is one demo box, not an HA service.
# Anything more elaborate would just add cost without changing the perf
# story we're trying to tell.

vpc = aws.ec2.Vpc(
    f"{prefix}-vpc",
    cidr_block="10.0.0.0/16",
    enable_dns_hostnames=True,
    enable_dns_support=True,
    tags={**tags, "Name": f"{prefix}-vpc"},
)

azs = aws.get_availability_zones(state="available")
subnet = aws.ec2.Subnet(
    f"{prefix}-subnet",
    vpc_id=vpc.id,
    cidr_block="10.0.1.0/24",
    availability_zone=azs.names[0],
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
aws.ec2.RouteTableAssociation(f"{prefix}-rt-assoc", subnet_id=subnet.id, route_table_id=rt.id)

# Public-facing demo, so the inbound rules are deliberately permissive.
# Tighten to your office IP via `pulumi config set ingress_cidr <your.ip/32>`
# if you don't want it open to the internet.
ingress_cidr = config.get("ingress_cidr") or "0.0.0.0/0"

sg = aws.ec2.SecurityGroup(
    f"{prefix}-sg",
    vpc_id=vpc.id,
    description="wingfoil baremetal demo — WS, prometheus, grafana, SSH",
    ingress=[
        aws.ec2.SecurityGroupIngressArgs(from_port=22,   to_port=22,   protocol="tcp", cidr_blocks=[ingress_cidr]),
        aws.ec2.SecurityGroupIngressArgs(from_port=8080, to_port=8080, protocol="tcp", cidr_blocks=[ingress_cidr]),
        aws.ec2.SecurityGroupIngressArgs(from_port=9091, to_port=9091, protocol="tcp", cidr_blocks=[ingress_cidr]),
        aws.ec2.SecurityGroupIngressArgs(from_port=3000, to_port=3000, protocol="tcp", cidr_blocks=[ingress_cidr]),
    ],
    egress=[aws.ec2.SecurityGroupEgressArgs(from_port=0, to_port=0, protocol="-1", cidr_blocks=["0.0.0.0/0"])],
    tags={**tags, "Name": f"{prefix}-sg"},
)

# ── S3 bucket: binaries + configs + static assets ────────────────────────
# user_data syncs the whole bucket down on first boot.

bucket = aws.s3.BucketV2(
    f"{prefix}-assets",
    force_destroy=True,  # demo stack — let `pulumi destroy` actually destroy
    tags=tags,
)

aws.s3.BucketObject(
    f"{prefix}-bin-ws-server",
    bucket=bucket.id,
    key="latency_e2e_ws_server",
    source=pulumi.FileAsset(str(WS_SERVER_BIN)),
)
aws.s3.BucketObject(
    f"{prefix}-bin-fix-gw",
    bucket=bucket.id,
    key="latency_e2e_fix_gw",
    source=pulumi.FileAsset(str(FIX_GW_BIN)),
)
aws.s3.BucketObject(
    f"{prefix}-idle-watch",
    bucket=bucket.id,
    key="idle_watch.sh",
    source=pulumi.FileAsset(str(HERE / "idle_watch.sh")),
)

# Static + observability configs — uploaded one object per file so cloud-init
# can grab the lot with `aws s3 sync`. Resource names are deterministic on
# the file path so Pulumi diffs cleanly when a config changes.
def upload_tree(local_dir: Path, key_prefix: str) -> None:
    if not local_dir.is_dir():
        return
    for path in sorted(local_dir.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(local_dir).as_posix()
        key = f"{key_prefix}/{rel}"
        # Pulumi requires unique resource names; derive from the key.
        resource_name = f"{prefix}-asset-{key}".replace("/", "-").replace(".", "-")
        content_type, _ = mimetypes.guess_type(path.name)
        aws.s3.BucketObject(
            resource_name,
            bucket=bucket.id,
            key=key,
            source=pulumi.FileAsset(str(path)),
            content_type=content_type or "application/octet-stream",
        )

upload_tree(EXAMPLE_DIR / "static",     "static")
upload_tree(EXAMPLE_DIR / "prometheus", "observability/prometheus")
upload_tree(EXAMPLE_DIR / "grafana",    "observability/grafana")
upload_tree(EXAMPLE_DIR / "tempo",      "observability/tempo")

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
        bucket.arn, lmax_username_secret.arn, lmax_password_secret.arn
    ).apply(lambda a: json.dumps({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["s3:GetObject", "s3:ListBucket"],
                "Resource": [a[0], f"{a[0]}/*"],
            },
            {
                "Effect": "Allow",
                "Action": ["secretsmanager:GetSecretValue"],
                "Resource": [a[1], a[2]],
            },
            # Self-stop: idle_watch.sh calls ec2:StopInstances on this
            # instance after 10 min of inactivity. Scoped via tag so the
            # box can't stop *other* instances in the account.
            {
                "Effect": "Allow",
                "Action": ["ec2:StopInstances"],
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

# ── AMI ──────────────────────────────────────────────────────────────────
# Canonical Ubuntu 22.04 LTS — predictable cloud-init, recent kernel for
# CPU isolation flags, available in every region.
ami = aws.ec2.get_ami(
    most_recent=True,
    owners=["099720109477"],  # Canonical
    filters=[
        aws.ec2.GetAmiFilterArgs(name="name", values=["ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*"]),
        aws.ec2.GetAmiFilterArgs(name="virtualization-type", values=["hvm"]),
    ],
)

# ── Bare-metal instance ──────────────────────────────────────────────────
user_data_template = (HERE / "user_data.sh").read_text()

def render_user_data(args):
    bucket_name, ws_arn, pw_arn = args
    return (
        user_data_template
        .replace("__ISOLATED_CORES__",       isolated_cores)
        .replace("__WS_SERVER_CORE__",       ws_server_core)
        .replace("__FIX_GW_CORE__",          fix_gw_core)
        .replace("__BINARIES_BUCKET__",      bucket_name)
        .replace("__LMAX_USERNAME_SECRET__", ws_arn)
        .replace("__LMAX_PASSWORD_SECRET__", pw_arn)
        .replace("__AWS_REGION__",           aws_region)
    )

user_data = pulumi.Output.all(
    bucket.bucket, lmax_username_secret.arn, lmax_password_secret.arn
).apply(render_user_data)

instance = aws.ec2.Instance(
    f"{prefix}-host",
    ami=ami.id,
    instance_type=instance_type,
    subnet_id=subnet.id,
    vpc_security_group_ids=[sg.id],
    iam_instance_profile=instance_profile.name,
    user_data=user_data,
    # Replace the box if user_data changes — otherwise edits to the script
    # silently never run on the existing instance.
    user_data_replace_on_change=True,
    root_block_device=aws.ec2.InstanceRootBlockDeviceArgs(
        volume_size=64,
        volume_type="gp3",
        delete_on_termination=True,
    ),
    tags={**tags, "Name": f"{prefix}-host"},
)

eip = aws.ec2.Eip(
    f"{prefix}-eip",
    instance=instance.id,
    domain="vpc",
    tags={**tags, "Name": f"{prefix}-eip"},
)

# ── Wake-on-demand Lambda ────────────────────────────────────────────────
# Public HTTPS function URL serving a "wake the box" page (GET /) plus a
# small JSON API (GET /status, POST /wake). No CORS — page and API live
# on the same origin. The lambda IAM role can only describe + start the
# specific instance ID, so a leaked URL can't grow to anything else in
# the account.

lambda_role = aws.iam.Role(
    f"{prefix}-wake-role",
    assume_role_policy=json.dumps({
        "Version": "2012-10-17",
        "Statement": [{
            "Action": "sts:AssumeRole",
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
        }],
    }),
    tags=tags,
)

aws.iam.RolePolicyAttachment(
    f"{prefix}-wake-logs",
    role=lambda_role.name,
    policy_arn="arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
)

aws.iam.RolePolicy(
    f"{prefix}-wake-policy",
    role=lambda_role.id,
    policy=instance.id.apply(lambda iid: json.dumps({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                # DescribeInstances doesn't support resource-level perms,
                # but the lambda only ever passes one InstanceIds= value
                # so the blast radius is purely informational.
                "Action": ["ec2:DescribeInstances"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": ["ec2:StartInstances"],
                "Resource": f"arn:aws:ec2:*:*:instance/{iid}",
            },
        ],
    })),
)

wake_lambda = aws.lambda_.Function(
    f"{prefix}-wake",
    role=lambda_role.arn,
    runtime="python3.12",
    handler="wake_lambda.handler",
    code=pulumi.AssetArchive({
        "wake_lambda.py": pulumi.FileAsset(str(HERE / "wake_lambda.py")),
    }),
    timeout=10,
    memory_size=128,
    # Hard ceiling on concurrent containers — caps total attack
    # throughput regardless of source IP count. Two is plenty for the
    # demo's request volume; legitimate users will never notice.
    reserved_concurrent_executions=2,
    environment=aws.lambda_.FunctionEnvironmentArgs(
        variables={"INSTANCE_ID": instance.id, "WS_SERVER_PORT": "8080"},
    ),
    tags=tags,
)

wake_url = aws.lambda_.FunctionUrl(
    f"{prefix}-wake-url",
    function_name=wake_lambda.name,
    authorization_type="NONE",
)

# ── Cost / abuse alarms ──────────────────────────────────────────────────
# Two-layer defence on top of the lambda concurrency cap:
#   (a) Real-time alarm on invocation rate — catches an ongoing attack
#       within minutes (not 24h like billing data).
#   (b) AWS Budget — backstop monthly cap with email at 80%% and 100%%.
# Both depend on `alert_email` being configured. Without it we skip
# alarm creation entirely (silent rather than half-broken).

if alert_email:
    alerts = aws.sns.Topic(f"{prefix}-alerts", tags=tags)
    aws.sns.TopicSubscription(
        f"{prefix}-alerts-email",
        topic=alerts.arn,
        protocol="email",
        endpoint=alert_email,
    )

    aws.cloudwatch.MetricAlarm(
        f"{prefix}-wake-rate-alarm",
        alarm_description="wingfoil baremetal — wake lambda hit >100 invocations in 5 min "
                          "(possible abuse). The lambda is concurrency-capped so this won't "
                          "actually drain your wallet, but worth a look.",
        comparison_operator="GreaterThanThreshold",
        evaluation_periods=1,
        period=300,
        threshold=100,
        statistic="Sum",
        namespace="AWS/Lambda",
        metric_name="Invocations",
        dimensions={"FunctionName": wake_lambda.name},
        alarm_actions=[alerts.arn],
        treat_missing_data="notBreaching",
        tags=tags,
    )

    # AWS Budgets lives in the account, not a region. Threshold is the
    # whole-account monthly forecast — for a single-stack account that's
    # effectively this stack's spend.
    aws.budgets.Budget(
        f"{prefix}-monthly-budget",
        budget_type="COST",
        limit_amount=monthly_budget_usd,
        limit_unit="USD",
        time_unit="MONTHLY",
        notifications=[
            aws.budgets.BudgetNotificationArgs(
                comparison_operator="GREATER_THAN",
                threshold=80,
                threshold_type="PERCENTAGE",
                notification_type="FORECASTED",
                subscriber_email_addresses=[alert_email],
            ),
            aws.budgets.BudgetNotificationArgs(
                comparison_operator="GREATER_THAN",
                threshold=100,
                threshold_type="PERCENTAGE",
                notification_type="ACTUAL",
                subscriber_email_addresses=[alert_email],
            ),
        ],
    )

# ── Outputs ──────────────────────────────────────────────────────────────
pulumi.export("instance_id",     instance.id)
pulumi.export("public_ip",       eip.public_ip)
pulumi.export("ws_server_url",   eip.public_ip.apply(lambda ip: f"http://{ip}:8080"))
pulumi.export("grafana_url",     eip.public_ip.apply(lambda ip: f"http://{ip}:3000"))
pulumi.export("prometheus_url",  eip.public_ip.apply(lambda ip: f"http://{ip}:9091/metrics"))
pulumi.export("assets_bucket",   bucket.bucket)
pulumi.export("wake_url",        wake_url.function_url)
