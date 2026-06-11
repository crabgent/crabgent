#!/usr/bin/env python3
"""Run Postgres integration tests against one reusable pgvector container."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import NoReturn

IMAGE = "pgvector/pgvector:pg18"
NAME = "crabgent-pgvector-test"
USER = "postgres"
PASSWORD = "postgres"
DATABASE = "postgres"
MAX_CONNECTIONS = 300
READY_TIMEOUT_SECS = 60
BASE_TEST_COMMAND = (
    "cargo",
    "nextest",
    "run",
    "--profile",
    "postgres-external",
)
DEFAULT_PACKAGE = "crabgent-store-postgres"


@dataclass(frozen=True)
class Config:
    image: str
    name: str
    max_connections: int
    port: int | None


def main() -> NoReturn:
    args = parse_args()
    config = Config(
        image=args.image,
        name=args.name,
        max_connections=args.max_connections,
        port=args.port,
    )

    require_docker()

    if args.action == "start":
        start(config, replace=args.replace)
        print(dsn(config.name), flush=True)
        raise SystemExit(0)
    if args.action == "stop":
        stop(config.name)
        raise SystemExit(0)
    if args.action == "status":
        print(status(config.name), flush=True)
        raise SystemExit(0)
    if args.action == "dsn":
        print(dsn(config.name), flush=True)
        raise SystemExit(0)

    start(config, replace=args.replace)
    env = os.environ.copy()
    env["PG_TEST_DSN"] = dsn(config.name)
    command = test_command(args)
    print(f"PG_TEST_DSN={env['PG_TEST_DSN']}", flush=True)
    raise SystemExit(subprocess.call(command, env=env))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Manage one local pgvector Docker container and run Postgres tests "
            "against it via PG_TEST_DSN."
        )
    )
    subparsers = parser.add_subparsers(dest="action", required=True)

    run_parser = subparsers.add_parser("run")
    add_common_args(run_parser)
    run_parser.add_argument(
        "--replace",
        action="store_true",
        help="Remove an existing container with the same name before starting.",
    )
    run_parser.add_argument(
        "--workspace",
        action="store_true",
        help="Run the full workspace with the postgres-external nextest profile.",
    )
    run_parser.add_argument(
        "-p",
        "--package",
        default=DEFAULT_PACKAGE,
        help=f"Package to run when --workspace is not set. Default: {DEFAULT_PACKAGE}.",
    )
    run_parser.add_argument(
        "nextest_args",
        nargs=argparse.REMAINDER,
        help="Additional cargo-nextest args. Prefix with -- before filters.",
    )

    start_parser = subparsers.add_parser("start")
    add_common_args(start_parser)
    start_parser.add_argument(
        "--replace",
        action="store_true",
        help="Remove an existing container with the same name before starting.",
    )

    for action in ("stop", "status", "dsn"):
        add_common_args(subparsers.add_parser(action))

    return parser.parse_args(sys.argv[1:] or ["run"])


def add_common_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--image", default=IMAGE)
    parser.add_argument("--name", default=NAME)
    parser.add_argument("--max-connections", type=int, default=MAX_CONNECTIONS)
    parser.add_argument(
        "--port",
        type=int,
        default=None,
        help="Optional fixed host port. Default asks Docker for a random port.",
    )


def require_docker() -> None:
    if shutil.which("docker") is None:
        fail("docker CLI not found in PATH")


def start(config: Config, *, replace: bool) -> None:
    if replace and container_exists(config.name):
        stop(config.name)

    if container_exists(config.name):
        if not container_running(config.name):
            run(["docker", "start", config.name])
        wait_ready(config.name)
        return

    port_arg = f"127.0.0.1:{config.port}:5432" if config.port else "127.0.0.1::5432"
    run(
        [
            "docker",
            "run",
            "--detach",
            "--name",
            config.name,
            "--env",
            f"POSTGRES_USER={USER}",
            "--env",
            f"POSTGRES_PASSWORD={PASSWORD}",
            "--env",
            f"POSTGRES_DB={DATABASE}",
            "--publish",
            port_arg,
            config.image,
            "-c",
            f"max_connections={config.max_connections}",
        ]
    )
    wait_ready(config.name)


def stop(name: str) -> None:
    if container_exists(name):
        run(["docker", "rm", "--force", name])


def status(name: str) -> str:
    if not container_exists(name):
        return f"{name}: absent"
    state = inspect(name, "{{.State.Status}}")
    image = inspect(name, "{{.Config.Image}}")
    if state == "running":
        return f"{name}: running image={image} dsn={dsn(name)}"
    return f"{name}: {state} image={image}"


def dsn(name: str) -> str:
    host, port = mapped_host_port(name)
    return f"postgres://{USER}:{PASSWORD}@{host}:{port}/{DATABASE}?sslmode=disable"


def mapped_host_port(name: str) -> tuple[str, str]:
    output = capture(["docker", "port", name, "5432/tcp"])
    line = output.splitlines()[-1] if output else ""
    if not line:
        fail(f"container {name} does not publish 5432/tcp")
    host, separator, port = line.rpartition(":")
    if not separator or not host or not port:
        fail(f"unexpected docker port output: {line!r}")
    if host in {"0.0.0.0", "::", "[::]"}:
        host = "127.0.0.1"
    return host, port


def wait_ready(name: str) -> None:
    deadline = time.monotonic() + READY_TIMEOUT_SECS
    while time.monotonic() < deadline:
        result = subprocess.run(
            ["docker", "exec", name, "pg_isready", "-U", USER, "-d", DATABASE],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode == 0:
            return
        time.sleep(0.5)

    logs = capture(["docker", "logs", "--tail", "80", name], check=False)
    fail(f"postgres did not become ready within {READY_TIMEOUT_SECS}s\n{logs}")


def container_exists(name: str) -> bool:
    return subprocess.run(
        ["docker", "inspect", name],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    ).returncode == 0


def container_running(name: str) -> bool:
    return inspect(name, "{{.State.Running}}") == "true"


def inspect(name: str, template: str) -> str:
    return capture(["docker", "inspect", "--format", template, name]).strip()


def test_command(args: argparse.Namespace) -> tuple[str, ...]:
    nextest_args = args.nextest_args
    if nextest_args and nextest_args[0] == "--":
        nextest_args = nextest_args[1:]
    scope = ("--workspace",) if args.workspace else ("-p", args.package)
    return (*BASE_TEST_COMMAND, *scope, *nextest_args)


def run(command: list[str]) -> None:
    subprocess.run(command, check=True)


def capture(command: list[str], *, check: bool = True) -> str:
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if check and result.returncode != 0:
        fail(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def fail(message: str) -> NoReturn:
    print(message, file=sys.stderr)
    raise SystemExit(1)


if __name__ == "__main__":
    main()
