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


rules: collections.Counter[str] = collections.Counter()
for sarif in sorted(scan_directory(sys.argv[1]).glob("*.sarif")):
    for result in json.loads(sarif.read_text())["runs"][0].get("results", []):
        where = result["locations"][0]["physicalLocation"]
        print(f"{result['ruleId']}  {where['artifactLocation']['uri']}:{where['region']['startLine']}")
        rules[result["ruleId"]] += 1
for rule, count in sorted(rules.items()):
    print(f"{count:4}  {rule}")
print(f"{sum(rules.values())} results")
sys.exit(1 if rules else 0)
