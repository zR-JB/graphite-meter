#!/usr/bin/env python3
"""The control plane's single GitHub CLI and JSON decoding boundary."""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import Mapping
from pathlib import Path
from typing import Protocol, TypeAlias, cast
from urllib.parse import urlencode

JsonScalar: TypeAlias = str | int | float | bool | None
JsonValue: TypeAlias = JsonScalar | list["JsonValue"] | dict[str, "JsonValue"]
JsonObject: TypeAlias = dict[str, JsonValue]
JsonArray: TypeAlias = list[JsonValue]


class ControlPlaneError(RuntimeError):
    pass


class GitHubAPIError(ControlPlaneError):
    pass


class JsonShapeError(ControlPlaneError):
    pass


class APICall(Protocol):
    def __call__(self, path: str, *, paginate: bool = False) -> JsonValue: ...


def api(path: str, *, paginate: bool = False) -> JsonValue:
    if not os.environ.get("GH_TOKEN"):
        raise GitHubAPIError("GH_TOKEN is required")
    pages = ["--paginate", "--slurp"] if paginate else []
    result = subprocess.run(["gh", "api", *pages, path], capture_output=True, text=True,
                            check=False)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise GitHubAPIError(f"gh api {path}: {detail}")
    text = result.stdout.strip()
    return decode_json(text, f"gh api {path}") if text else None


def query(path: str, **params: str | int) -> str:
    return f"{path}?{urlencode(params)}"


def decode_json(text: str, context: str) -> JsonValue:
    try:
        return cast(JsonValue, json.loads(text))
    except json.JSONDecodeError as exc:
        raise JsonShapeError(f"{context} is not valid JSON: {exc}") from exc


def expect_object(value: JsonValue, context: str) -> JsonObject:
    if not isinstance(value, dict):
        raise JsonShapeError(f"{context} must be a JSON object")
    return value


def expect_array(value: JsonValue, context: str) -> JsonArray:
    if not isinstance(value, list):
        raise JsonShapeError(f"{context} must be a JSON array")
    return value


def object_field(value: Mapping[str, JsonValue], key: str, context: str) -> JsonObject:
    return expect_object(value.get(key), f"{context}.{key}")


def str_field(value: Mapping[str, JsonValue], key: str, context: str) -> str:
    item = value.get(key)
    if not isinstance(item, str):
        raise JsonShapeError(f"{context}.{key} must be a string")
    return item


def int_field(value: Mapping[str, JsonValue], key: str, context: str) -> int:
    item = value.get(key)
    if not isinstance(item, int) or isinstance(item, bool):
        raise JsonShapeError(f"{context}.{key} must be an integer")
    return item


def append_output(**values: object) -> None:
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as handle:
        for key, value in values.items():
            if "\n" in (text := str(value)) or "\r" in text:
                raise ValueError(f"output {key!r} must be single-line")
            handle.write(f"{key}={text}\n")


def append_summary(text: str) -> None:
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as handle:
        handle.write(text.rstrip() + "\n")
