#!/usr/bin/env python3
"""Typed, dependency-free GitHub CLI/JSON helpers for the CI control plane.

`gh api` owns authentication and HTTP behavior. This module defines the single
JSON boundary: JSON is decoded once, then critical callers narrow values with
small runtime validators before policy code consumes them.
"""

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
QueryValue: TypeAlias = str | int | bool | None


class GitHubAPIError(RuntimeError):
    pass


class JsonShapeError(ValueError):
    pass


class APICall(Protocol):
    def __call__(
        self,
        path: str,
        *,
        method: str = "GET",
        payload: JsonValue | None = None,
        paginate: bool = False,
    ) -> JsonValue: ...


def api(
    path: str,
    *,
    method: str = "GET",
    payload: JsonValue | None = None,
    paginate: bool = False,
) -> JsonValue:
    if not os.environ.get("GH_TOKEN"):
        raise GitHubAPIError("GH_TOKEN is required")

    args = ["gh", "api"]
    if method != "GET":
        args += ["--method", method]
    if paginate:
        args += ["--paginate", "--slurp"]
    if payload is not None:
        args += ["--input", "-"]
    args.append(path)

    result = subprocess.run(
        args,
        input=(json.dumps(payload, separators=(",", ":")) if payload is not None else None),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise GitHubAPIError(f"gh api {method} {path}: {detail}")

    text = result.stdout.strip()
    if not text:
        return None
    try:
        return decode_json(text, f"gh api {method} {path}")
    except JsonShapeError as exc:
        raise GitHubAPIError(str(exc)) from exc


def query(path: str, **params: QueryValue) -> str:
    filtered: dict[str, str | int | bool] = {
        key: value for key, value in params.items() if value is not None
    }
    return f"{path}?{urlencode(filtered, doseq=True)}" if filtered else path


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


def array_field(value: Mapping[str, JsonValue], key: str, context: str) -> JsonArray:
    return expect_array(value.get(key), f"{context}.{key}")


def str_field(value: Mapping[str, JsonValue], key: str, context: str) -> str:
    item = value.get(key)
    if not isinstance(item, str):
        raise JsonShapeError(f"{context}.{key} must be a string")
    return item


def optional_str_field(value: Mapping[str, JsonValue], key: str, context: str) -> str | None:
    item = value.get(key)
    if item is None:
        return None
    if not isinstance(item, str):
        raise JsonShapeError(f"{context}.{key} must be a string or null")
    return item


def int_field(value: Mapping[str, JsonValue], key: str, context: str) -> int:
    item = value.get(key)
    if not isinstance(item, int) or isinstance(item, bool):
        raise JsonShapeError(f"{context}.{key} must be an integer")
    return item


def append_output(**values: object) -> None:
    output_path = os.environ.get("GITHUB_OUTPUT")
    if not output_path:
        raise RuntimeError("GITHUB_OUTPUT is required")
    with Path(output_path).open("a", encoding="utf-8") as handle:
        for key, value in values.items():
            text = str(value)
            if "\n" in text or "\r" in text:
                raise ValueError(f"output {key!r} must be single-line")
            handle.write(f"{key}={text}\n")


def append_summary(text: str) -> None:
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary_path:
        raise RuntimeError("GITHUB_STEP_SUMMARY is required")
    with Path(summary_path).open("a", encoding="utf-8") as handle:
        handle.write(text.rstrip() + "\n")
