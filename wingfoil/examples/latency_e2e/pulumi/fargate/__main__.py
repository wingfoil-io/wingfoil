import json
import pulumi
import pulumi_aws as aws

# Configuration
config = pulumi.Config()
project_name = pulumi.get_project()
environment = config.get("environment") or "demo"
aws_region = config.require("aws:region")
ws_server_image = config.require("ws_server_image")
fix_gw_image = config.require("fix_gw_image")
prometheus_image = config.require("prometheus_image")
tempo_image = config.require("tempo_image")
grafana_image = config.require("grafana_image")
lmax_username = config.require_secret("lmax_username")
lmax_password = config.require_secret("lmax_password")
cpu = config.get_int("cpu") or 1024
memory = config.get_int("memory") or 2048
shm_size = config.get_int("shm_size") or 256

tags = {
    "Project": project_name,
    "Environment": environment,
    "ManagedBy": "Pulumi",
}

# VPC
vpc = aws.ec2.Vpc(f"{project_name}-vpc",
    cidr_block="10.0.0.0/16",
    enable_dns_hostnames=True,
    enable_dns_support=True,
    tags={"Name": f"{project_name}-vpc", **tags})

# Public subnet
public_subnet = aws.ec2.Subnet(f"{project_name}-public-subnet",
    vpc_id=vpc.id,
    cidr_block="10.0.1.0/24",
    availability_zone=f"{aws_region}a",
    map_public_ip_on_launch=True,
    tags={"Name": f"{project_name}-public-subnet", **tags})

# Private subnet
private_subnet = aws.ec2.Subnet(f"{project_name}-private-subnet",
    vpc_id=vpc.id,
    cidr_block="10.0.2.0/24",
    availability_zone=f"{aws_region}a",
    tags={"Name": f"{project_name}-private-subnet", **tags})

# Internet Gateway
igw = aws.ec2.InternetGateway(f"{project_name}-igw",
    vpc_id=vpc.id,
    tags={"Name": f"{project_name}-igw", **tags})

# NAT Gateway
eip = aws.ec2.Eip(f"{project_name}-nat-eip",
    domain="vpc",
    tags={"Name": f"{project_name}-nat-eip", **tags},
    depends_on=[igw])

nat = aws.ec2.NatGateway(f"{project_name}-nat",
    subnet_id=public_subnet.id,
    allocation_id=eip.id,
    tags={"Name": f"{project_name}-nat", **tags})

# Public route table
public_rt = aws.ec2.RouteTable(f"{project_name}-public-rt",
    vpc_id=vpc.id,
    routes=[
        aws.ec2.RouteTableRouteArgs(
            cidr_block="0.0.0.0/0",
            gateway_id=igw.id,
        )
    ],
    tags={"Name": f"{project_name}-public-rt", **tags})

public_rt_assoc = aws.ec2.RouteTableAssociation(f"{project_name}-public-rt-assoc",
    subnet_id=public_subnet.id,
    route_table_id=public_rt.id)

# Private route table
private_rt = aws.ec2.RouteTable(f"{project_name}-private-rt",
    vpc_id=vpc.id,
    routes=[
        aws.ec2.RouteTableRouteArgs(
            cidr_block="0.0.0.0/0",
            nat_gateway_id=nat.id,
        )
    ],
    tags={"Name": f"{project_name}-private-rt", **tags})

private_rt_assoc = aws.ec2.RouteTableAssociation(f"{project_name}-private-rt-assoc",
    subnet_id=private_subnet.id,
    route_table_id=private_rt.id)

# Security groups
alb_sg = aws.ec2.SecurityGroup(f"{project_name}-alb-sg",
    vpc_id=vpc.id,
    description="Security group for ALB",
    ingress=[
        aws.ec2.SecurityGroupIngressArgs(from_port=80, to_port=80, protocol="tcp", cidr_blocks=["0.0.0.0/0"]),
        aws.ec2.SecurityGroupIngressArgs(from_port=443, to_port=443, protocol="tcp", cidr_blocks=["0.0.0.0/0"]),
        aws.ec2.SecurityGroupIngressArgs(from_port=3000, to_port=3000, protocol="tcp", cidr_blocks=["0.0.0.0/0"]),
    ],
    egress=[aws.ec2.SecurityGroupEgressArgs(from_port=0, to_port=0, protocol="-1", cidr_blocks=["0.0.0.0/0"])],
    tags={"Name": f"{project_name}-alb-sg", **tags})

ecs_sg = aws.ec2.SecurityGroup(f"{project_name}-ecs-tasks-sg",
    vpc_id=vpc.id,
    description="Security group for ECS tasks",
    ingress=[
        aws.ec2.SecurityGroupIngressArgs(from_port=8080, to_port=8080, protocol="tcp", security_groups=[alb_sg.id]),
        aws.ec2.SecurityGroupIngressArgs(from_port=9091, to_port=9091, protocol="tcp", security_groups=[alb_sg.id]),
        aws.ec2.SecurityGroupIngressArgs(from_port=3000, to_port=3000, protocol="tcp", security_groups=[alb_sg.id]),
        aws.ec2.SecurityGroupIngressArgs(from_port=4318, to_port=4318, protocol="tcp", security_groups=[alb_sg.id]),
    ],
    egress=[aws.ec2.SecurityGroupEgressArgs(from_port=0, to_port=0, protocol="-1", cidr_blocks=["0.0.0.0/0"])],
    tags={"Name": f"{project_name}-ecs-tasks-sg", **tags})

# CloudWatch log group
log_group = aws.cloudwatch.LogGroup(f"{project_name}-ecs-logs",
    name=f"/ecs/{project_name}-{environment}",
    retention_in_days=7,
    tags=tags)

# Secrets Manager
lmax_username_secret = aws.secretsmanager.Secret(f"{project_name}-lmax-username",
    description="LMAX FIX username",
    tags=tags)

lmax_username_version = aws.secretsmanager.SecretVersion(f"{project_name}-lmax-username-version",
    secret_id=lmax_username_secret.id,
    secret_string=lmax_username)

lmax_password_secret = aws.secretsmanager.Secret(f"{project_name}-lmax-password",
    description="LMAX FIX password",
    tags=tags)

lmax_password_version = aws.secretsmanager.SecretVersion(f"{project_name}-lmax-password-version",
    secret_id=lmax_password_secret.id,
    secret_string=lmax_password)

# IAM role for ECS task
ecs_task_role = aws.iam.Role(f"{project_name}-ecs-task-role",
    assume_role_policy=json.dumps({
        "Version": "2012-10-17",
        "Statement": [{
            "Action": "sts:AssumeRole",
            "Effect": "Allow",
            "Principal": {"Service": "ecs-tasks.amazonaws.com"}
        }]
    }),
    tags=tags)

# IAM policy for Secrets Manager access
secrets_policy = aws.iam.RolePolicy(f"{project_name}-ecs-secrets-policy",
    role=ecs_task_role.id,
    policy=pulumi.Output.all(lmax_username_secret.arn, lmax_password_secret.arn).apply(
        lambda arns: json.dumps({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": ["secretsmanager:GetSecretValue"],
                "Resource": arns
            }]
        })))

# ECS Cluster
cluster = aws.ecs.Cluster(f"{project_name}-cluster",
    tags=tags)

# ECS Task Execution Role (for logging, pulling images, etc.)
ecs_task_execution_role = aws.iam.Role(f"{project_name}-ecs-task-execution-role",
    assume_role_policy=json.dumps({
        "Version": "2012-10-17",
        "Statement": [{
            "Action": "sts:AssumeRole",
            "Effect": "Allow",
            "Principal": {"Service": "ecs-tasks.amazonaws.com"}
        }]
    }),
    tags=tags)

ecs_task_execution_policy = aws.iam.RolePolicyAttachment(f"{project_name}-ecs-task-execution-policy",
    role=ecs_task_execution_role.name,
    policy_arn="arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy")

# ALB
alb = aws.lb.LoadBalancer(f"{project_name}-alb",
    internal=False,
    load_balancer_type="application",
    security_groups=[alb_sg.id],
    subnets=[public_subnet.id, private_subnet.id],
    enable_deletion_protection=False,
    tags={"Name": f"{project_name}-alb", **tags})

# Target group for WS server
ws_target_group = aws.lb.TargetGroup(f"{project_name}-ws-tg",
    port=8080,
    protocol="HTTP",
    vpc_id=vpc.id,
    target_type="ip",
    health_check=aws.lb.TargetGroupHealthCheckArgs(
        healthy_threshold=2,
        unhealthy_threshold=2,
        timeout=3,
        interval=30,
        path="/",
        matcher="200",
    ),
    tags={"Name": f"{project_name}-ws-tg", **tags})

# Target group for Grafana
grafana_target_group = aws.lb.TargetGroup(f"{project_name}-grafana-tg",
    port=3000,
    protocol="HTTP",
    vpc_id=vpc.id,
    target_type="ip",
    health_check=aws.lb.TargetGroupHealthCheckArgs(
        healthy_threshold=2,
        unhealthy_threshold=2,
        timeout=3,
        interval=30,
        path="/api/health",
        matcher="200",
    ),
    tags={"Name": f"{project_name}-grafana-tg", **tags})

# ALB listeners
ws_listener = aws.lb.Listener(f"{project_name}-ws-listener",
    load_balancer_arn=alb.arn,
    port=80,
    protocol="HTTP",
    default_actions=[
        aws.lb.ListenerDefaultActionArgs(
            type="forward",
            target_group_arn=ws_target_group.arn,
        )
    ])

grafana_listener = aws.lb.Listener(f"{project_name}-grafana-listener",
    load_balancer_arn=alb.arn,
    port=3000,
    protocol="HTTP",
    default_actions=[
        aws.lb.ListenerDefaultActionArgs(
            type="forward",
            target_group_arn=grafana_target_group.arn,
        )
    ])

# ECS Task Definition
task_definition = aws.ecs.TaskDefinition(f"{project_name}-task",
    family=f"{project_name}-task",
    network_mode="awsvpc",
    requires_compatibilities=["FARGATE"],
    cpu=str(cpu),
    memory=str(memory),
    execution_role_arn=ecs_task_execution_role.arn,
    task_role_arn=ecs_task_role.arn,
    container_definitions=pulumi.Output.all(
        log_group.name,
        lmax_username_secret.arn,
        lmax_password_secret.arn,
    ).apply(lambda args: json.dumps([
        # ws_server container
        {
            "name": "ws_server",
            "image": ws_server_image,
            "portMappings": [
                {"containerPort": 8080, "hostPort": 8080, "protocol": "tcp"},
                {"containerPort": 9091, "hostPort": 9091, "protocol": "tcp"},
            ],
            "environment": [
                {"name": "WINGFOIL_WEB_ADDR", "value": "0.0.0.0:8080"},
                {"name": "WINGFOIL_METRICS_ADDR", "value": "0.0.0.0:9091"},
                {"name": "WINGFOIL_OTLP_ENDPOINT", "value": "http://localhost:4318"},
                {"name": "RUST_LOG", "value": "info"},
            ],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": args[0],
                    "awslogs-region": aws_region,
                    "awslogs-stream-prefix": "ws_server",
                }
            }
        },
        # fix_gw container
        {
            "name": "fix_gw",
            "image": fix_gw_image,
            "environment": [
                {"name": "RUST_LOG", "value": "info"},
            ],
            "secrets": [
                {"name": "LMAX_USERNAME", "valueFrom": f"{args[1]}:LMAX_USERNAME::"},
                {"name": "LMAX_PASSWORD", "valueFrom": f"{args[2]}:LMAX_PASSWORD::"},
            ],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": args[0],
                    "awslogs-region": aws_region,
                    "awslogs-stream-prefix": "fix_gw",
                }
            }
        },
        # Prometheus container (config baked into the image)
        {
            "name": "prometheus",
            "image": prometheus_image,
            "portMappings": [
                {"containerPort": 9090, "hostPort": 9090, "protocol": "tcp"},
            ],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": args[0],
                    "awslogs-region": aws_region,
                    "awslogs-stream-prefix": "prometheus",
                }
            }
        },
        # Tempo container (config baked into the image; local-filesystem backend
        # — traces don't need to outlive the task for this demo).
        {
            "name": "tempo",
            "image": tempo_image,
            "portMappings": [
                {"containerPort": 3200, "hostPort": 3200, "protocol": "tcp"},
                {"containerPort": 4318, "hostPort": 4318, "protocol": "tcp"},
            ],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": args[0],
                    "awslogs-region": aws_region,
                    "awslogs-stream-prefix": "tempo",
                }
            }
        },
        # Grafana container (provisioning baked into the image)
        {
            "name": "grafana",
            "image": grafana_image,
            "portMappings": [
                {"containerPort": 3000, "hostPort": 3000, "protocol": "tcp"},
            ],
            "environment": [
                {"name": "GF_SECURITY_ADMIN_PASSWORD", "value": "admin"},
                {"name": "GF_AUTH_ANONYMOUS_ENABLED", "value": "true"},
                {"name": "GF_AUTH_ANONYMOUS_ORG_ROLE", "value": "Viewer"},
                {"name": "GF_AUTH_DISABLE_LOGIN_FORM", "value": "true"},
                {"name": "GF_SECURITY_ALLOW_EMBEDDING", "value": "true"},
                {"name": "GF_DASHBOARDS_DEFAULT_HOME_DASHBOARD_PATH", "value": "/etc/grafana/provisioning/dashboards/latency.json"},
                {"name": "GF_FEATURE_TOGGLES_ENABLE", "value": "traceqlEditor"},
            ],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": args[0],
                    "awslogs-region": aws_region,
                    "awslogs-stream-prefix": "grafana",
                }
            }
        }
    ])),
    tags=tags)

# ECS Service
service = aws.ecs.Service(f"{project_name}-service",
    cluster=cluster.arn,
    task_definition=task_definition.arn,
    desired_count=1,
    launch_type="FARGATE",
    network_configuration=aws.ecs.ServiceNetworkConfigurationArgs(
        subnets=[private_subnet.id],
        security_groups=[ecs_sg.id],
        assign_public_ip=False,
    ),
    load_balancers=[
        aws.ecs.ServiceLoadBalancerArgs(
            target_group_arn=ws_target_group.arn,
            container_name="ws_server",
            container_port=8080,
        ),
        aws.ecs.ServiceLoadBalancerArgs(
            target_group_arn=grafana_target_group.arn,
            container_name="grafana",
            container_port=3000,
        ),
    ],
    tags=tags,
    opts=pulumi.ResourceOptions(depends_on=[ws_listener, grafana_listener]))

# Outputs
pulumi.export("alb_dns_name", alb.dns_name)
pulumi.export("alb_arn", alb.arn)
pulumi.export("ecs_cluster_name", cluster.name)
pulumi.export("ecs_service_name", service.name)
pulumi.export("cloudwatch_log_group", log_group.name)
