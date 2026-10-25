#!/usr/bin/env python3
"""Shared control-plane boundaries: the GitHub CLI, JSON decoding, paths and key material."""

import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from collections import Counter
from collections.abc import Mapping
from pathlib import Path
from typing import NoReturn, TypeAlias, cast
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
JSON_STRING = re.compile(r'"[^"\\]*(?:\\.[^"\\]*)*"')
# Far deeper than any GitHub or OCI document, so the limit and not the interpreter's stack refuses one.
JSON_DEPTH = 64
TIMEOUT_SECONDS, UPLOAD_TIMEOUT_SECONDS = 60, 600
RETRY_SECONDS = (2, 10)
REPEATABLE = ("GET", "PATCH", "DELETE")
STATUS = re.compile(r"\bHTTP (\d{3})\b")
DROPPED = re.compile(r"connection reset|timeout|\bEOF\b", re.IGNORECASE)


class ControlPlaneError(RuntimeError):
    pass


def fail(message: str) -> NoReturn:
    raise ControlPlaneError(message)


def api(path: str, *, paginate: bool = False, method: str = "GET", body: JsonValue = None,
        upload: Path | None = None) -> JsonValue:
    """Call `gh api` with the token in GH_TOKEN, sending `body` as JSON or `upload` as bytes; a POST never repeats."""
    if not os.environ.get("GH_TOKEN"):
        fail("GH_TOKEN is required")
    args = ["--paginate", "--slurp"] if paginate else []
    if method != "GET":
        args += ["--method", method]
    if upload is not None:
        args += ["--header", "Content-Type: application/octet-stream", "--input", str(upload)]
    elif body is not None:
        args += ["--input", "-"]
    timeout = TIMEOUT_SECONDS if upload is None else UPLOAD_TIMEOUT_SECONDS
    delays = iter(RETRY_SECONDS if method in REPEATABLE else ())
    while True:
        try:
            result = subprocess.run(["gh", "api", *args, path], input=None if body is None else json.dumps(body),
                                    capture_output=True, text=True, check=False, timeout=timeout)
        except subprocess.TimeoutExpired:
            detail = f"timeout after {timeout} s"
        else:
            if result.returncode == 0:
                text = result.stdout.strip()
                return decode_json(text, f"gh api {path}") if text else None
            detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        status = STATUS.search(detail)
        transient = status[1] == "429" or status[1][0] == "5" if status else DROPPED.search(detail)
        if not transient or (delay := next(delays, None)) is None:
            fail(f"gh api {path}: {detail}")
        print(f"::warning::gh api {path}: {detail}; retrying in {delay} s", file=sys.stderr)
        time.sleep(delay)


def query(path: str, **params: str | int) -> str:
    return f"{path}?{urlencode(params)}"


def distinct_keys(pairs: list[tuple[str, JsonValue]]) -> JsonObject:
    # Case-insensitive decoders bind every case variant of a key to one field, so variants must not coexist.
    folded = Counter(key.casefold() for key, _ in pairs)
    if repeated := sorted(key for key, _ in pairs if folded[key.casefold()] > 1):
        raise ValueError(f"keys {repeated} repeat ignoring case")
    return dict(pairs)


def nonfinite(name: str) -> NoReturn:
    raise ValueError(f"{name} is not a JSON number")


def decode_json(text: str, context: str) -> JsonValue:
    depth = 0
    for bracket in re.findall(r"[][{}]", JSON_STRING.sub("", text)):
        depth += 1 if bracket in "[{" else -1
        if depth > JSON_DEPTH:
            fail(f"{context} nests deeper than {JSON_DEPTH} levels")
    try:
        return cast(JsonValue, json.loads(text, object_pairs_hook=distinct_keys, parse_constant=nonfinite))
    except ValueError as exc:
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


def confined_path(value: str | Path, *roots: str | Path) -> Path:
    """Resolve `value`, following links, and require it to lie strictly inside one of `roots`."""
    path = os.path.realpath(value)
    if path.startswith(tuple(os.path.join(os.path.realpath(root), "") for root in roots)):
        return Path(path)
    raise ControlPlaneError(f"{value} is outside {', '.join(map(str, roots)) or 'every allowed root'}")


def local_path(value: str | Path, base: str | Path) -> Path:
    """Resolve `value`, which must lie inside `base`, the temporary directory or RUNNER_TEMP."""
    return confined_path(value, base, tempfile.gettempdir(), os.environ.get("RUNNER_TEMP") or base)


def runner_path(name: str) -> Path:
    """Return the path in environment variable `name`, which must lie inside RUNNER_TEMP."""
    value, root = os.environ.get(name), os.environ.get("RUNNER_TEMP")
    if not value or not root:
        raise ControlPlaneError(f"{name} and RUNNER_TEMP are required")
    return confined_path(value, root)


def append_output(**values: object) -> None:
    with runner_path("GITHUB_OUTPUT").open("a") as handle:
        for key, value in values.items():
            if "\n" in (text := str(value)) or "\r" in text:
                raise ValueError(f"output {key!r} must be single-line")
            handle.write(f"{key}={text}\n")


def append_summary(text: str) -> None:
    with runner_path("GITHUB_STEP_SUMMARY").open("a") as handle:
        handle.write(text.rstrip() + "\n")


def table(rows: list[tuple[str, str]]) -> str:
    """A Markdown table whose first row is its header; cells hold only validated values."""
    lines = [f"| {left} | {right} |" for left, right in rows]
    return "\n".join([lines[0], "|---|---|", *lines[1:]])


def file_sha256(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()
