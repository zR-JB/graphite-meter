#!/usr/bin/env python3
"""List the SARIF results in a scan directory and count them per rule; fail when there are any."""

from __future__ import annotations

import collections
import json
import os
import sys
import tempfile
from pathlib import Path


def scan_directory(value: str) -> Path:
    path, temp = os.path.realpath(value), os.path.join(os.path.realpath(tempfile.gettempdir()), "")
    if path.startswith(temp):
        return Path(path)
    sys.exit(f"{value} is outside {temp}")


scan = scan_directory(sys.argv[1])
failed = False
rust_seen = False
rules: collections.Counter[str] = collections.Counter()
for sarif in sorted(scan.glob("*.sarif")):
    run = json.loads(sarif.read_text())["runs"][0]
    if sarif.stem == "rust":
        rust_seen = True
        expected = {str(path.relative_to(scan / "src/rust")) for path in (scan / "src/rust").rglob("*.rs")}
        extracted = set()
        for invocation in run.get("invocations", []):
            for diagnostic in invocation.get("toolExecutionNotifications", []):
                identity = diagnostic.get("descriptor", {}).get("id", "")
                if identity == "rust/diagnostics/successfully-extracted-files":
                    extracted.update(location["physicalLocation"]["artifactLocation"]["uri"] for location in diagnostic.get("locations", []))
                elif identity in {"rust/diagnostics/extraction-errors", "rust/diagnostics/extraction-warnings", "rust/diagnostics/unresolved-macro-calls"}:
                    print(diagnostic["message"]["text"], file=sys.stderr)
                    failed = True
        metrics = {metric["ruleId"]: metric["value"] for metric in run.get("properties", {}).get("metricResults", [])}
        errors = metrics.get("rust/summary/number-of-files-extracted-with-errors")
        successful = metrics.get("rust/summary/number-of-successfully-extracted-files")
        missing = expected - extracted
        if errors != 0 or successful != len(expected) or missing or not expected:
            print(f"Rust coverage failed: {successful}/{len(expected)} files, {errors} files with errors; missing {sorted(missing)}", file=sys.stderr)
            failed = True
        else:
            print(f"Rust coverage: {successful}/{len(expected)} files, 0 files with errors")
    for result in run.get("results", []):
        where = result["locations"][0]["physicalLocation"]
        print(f"{result['ruleId']}  {where['artifactLocation']['uri']}:{where['region']['startLine']}")
        rules[result["ruleId"]] += 1
if (scan / "src/rust").is_dir() and not rust_seen:
    print("Rust coverage failed: rust.sarif is missing", file=sys.stderr)
    failed = True
for rule, count in sorted(rules.items()):
    print(f"{count:4}  {rule}")
print(f"{sum(rules.values())} results")
sys.exit(1 if rules or failed else 0)
