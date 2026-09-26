#!/usr/bin/env python3
"""List CodeQL SARIF results and count them per rule; fail when there are any."""

from __future__ import annotations

import collections
import json
import sys
from pathlib import Path

rules: collections.Counter[str] = collections.Counter()
for name in sys.argv[1:]:
    for result in json.loads(Path(name).read_text())["runs"][0].get("results", []):
        where = result["locations"][0]["physicalLocation"]
        print(f"{result['ruleId']}  {where['artifactLocation']['uri']}:{where['region']['startLine']}")
        rules[result["ruleId"]] += 1
for rule, count in sorted(rules.items()):
    print(f"{count:4}  {rule}")
print(f"{sum(rules.values())} results")
sys.exit(1 if rules else 0)
