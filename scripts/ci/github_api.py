#!/usr/bin/env python3
"""Shared control-plane boundaries: the GitHub CLI, JSON decoding, paths and key material."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
from collections.abc import Mapping
from pathlib import Path
from typing import NoReturn, Protocol, TypeAlias, cast
from urllib.parse import urlencode

JsonScalar: TypeAlias = str | int | float | bool | None
JsonValue: TypeAlias = JsonScalar | list["JsonValue"] | dict[str, "JsonValue"]
JsonObject: TypeAlias = dict[str, JsonValue]
JsonArray: TypeAlias = list[JsonValue]
TLS_NAME = re.compile(
    r"(^|/)(\.dev-certs|certs?|certificates?|letsencrypt)(/|$)|"
    r"\.(pem|key|crt|cer|der|csr|p12|pfx|pkcs8|jks|keystore)$",
    re.IGNORECASE,
)
PEM = re.compile(rb"-----BEGIN (?:CERTIFICATE|(?:[^ -]+ )*PRIVATE KEY)-----")


class ControlPlaneError(RuntimeError):
    pass


def fail(message: str) -> NoReturn:
    raise ControlPlaneError(message)


class APICall(Protocol):
    def __call__(self, path: str, *, paginate: bool = False) -> JsonValue:
        """Return the decoded JSON at `path`, every page of it when `paginate` is set."""


def api(path: str, *, paginate: bool = False) -> JsonValue:
    if not os.environ.get("GH_TOKEN"):
        fail("GH_TOKEN is required")
    pages = ["--paginate", "--slurp"] if paginate else []
    result = subprocess.run(["gh", "api", *pages, path], capture_output=True, text=True,
                            check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        fail(f"gh api {path}: {detail}")
    text = result.stdout.strip()
    return decode_json(text, f"gh api {path}") if text else None


def query(path: str, **params: str | int) -> str:
    return f"{path}?{urlencode(params)}"


def decode_json(text: str, context: str) -> JsonValue:
    try:
        return cast(JsonValue, json.loads(text))
    except json.JSONDecodeError as exc:
        raise ControlPlaneError(f"{context} is not valid JSON: {exc}") from exc


def expect_object(value: JsonValue, context: str) -> JsonObject:
    if not isinstance(value, dict):
        fail(f"{context} must be a JSON object")
    return value


def expect_array(value: JsonValue, context: str) -> JsonArray:
    if not isinstance(value, list):
        fail(f"{context} must be a JSON array")
    return value


def object_field(value: Mapping[str, JsonValue], key: str, context: str) -> JsonObject:
    return expect_object(value.get(key), f"{context}.{key}")


def str_field(value: Mapping[str, JsonValue], key: str, context: str) -> str:
    item = value.get(key)
    if not isinstance(item, str):
        fail(f"{context}.{key} must be a string")
    return item


def int_field(value: Mapping[str, JsonValue], key: str, context: str) -> int:
    item = value.get(key)
    if not isinstance(item, int) or isinstance(item, bool):
        fail(f"{context}.{key} must be an integer")
    return item


def confined_path(value: str, *roots: str) -> Path:
    """Resolve `value`, following links, and require it to lie strictly inside one of `roots`."""
    path = os.path.realpath(value)
    if path.startswith(tuple(os.path.join(os.path.realpath(root), "") for root in roots)):
        return Path(path)
    raise ControlPlaneError(f"{value} is outside {', '.join(roots) or 'every allowed root'}")


def runner_path(name: str) -> Path:
    """Return the path in environment variable `name`, which must lie inside RUNNER_TEMP."""
    value, root = os.environ.get(name), os.environ.get("RUNNER_TEMP")
    if not value or not root:
        raise ControlPlaneError(f"{name} and RUNNER_TEMP are required")
    return confined_path(value, root)


def append_output(**values: object) -> None:
    with runner_path("GITHUB_OUTPUT").open("a", encoding="utf-8") as handle:
        for key, value in values.items():
            if "\n" in (text := str(value)) or "\r" in text:
                raise ValueError(f"output {key!r} must be single-line")
            handle.write(f"{key}={text}\n")


def append_summary(text: str) -> None:
    with runner_path("GITHUB_STEP_SUMMARY").open("a", encoding="utf-8") as handle:
        handle.write(text.rstrip() + "\n")


def file_sha256(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()
