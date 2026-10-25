import json
import re
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any

API = Path(__file__).resolve().parents[2] / "api"
ANNOTATIONS = {"$schema", "$id", "$defs", "title", "description"}
KEYWORDS = {"$ref", "type", "enum", "minLength", "maxLength", "pattern", "minimum", "maximum", "minItems",
            "maxItems", "uniqueItems", "items", "required", "properties", "additionalProperties"}
TYPES: dict[str, type] = {"object": dict, "array": list, "string": str, "integer": int, "boolean": bool}


def require_supported(schema: dict[str, Any]) -> None:
    unknown = schema.keys() - ANNOTATIONS - KEYWORDS
    if schema.get("type", "object") not in TYPES:
        unknown.add("type")
    if not schema.get("$ref", "#/$defs/").startswith("#/$defs/"):
        unknown.add("$ref")
    if schema.get("additionalProperties", False) is not False:
        unknown.add("additionalProperties")
    if unknown:
        raise ValueError(f"unsupported schema keywords {sorted(unknown)}")
    items = [schema["items"]] if "items" in schema else []
    for child in [*schema.get("properties", {}).values(), *schema.get("$defs", {}).values(), *items]:
        require_supported(child)


def canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True)


def check(schema: dict[str, Any], value: Any, root: dict[str, Any], at: str = "$") -> None:
    def fail(rule: str) -> None:
        raise ValueError(f"{at}: {rule}")

    if "$ref" in schema:
        name = schema["$ref"].removeprefix("#/$defs/")
        check(root["$defs"][name], value, root, at)
    kind = TYPES.get(schema.get("type", ""), object)
    if not isinstance(value, kind) or (kind is int and isinstance(value, bool)):
        fail(f"not {schema['type']}")
    if "enum" in schema and canonical(value) not in map(canonical, schema["enum"]):
        fail(f"{value!r} not in enum")
    if isinstance(value, str):
        if not schema.get("minLength", 0) <= len(value) <= schema.get("maxLength", len(value)):
            fail("length out of bounds")
        if "pattern" in schema and not re.search(schema["pattern"], value):
            fail(f"{value!r} does not match {schema['pattern']}")
    if isinstance(value, int) and not isinstance(value, bool):
        if not schema.get("minimum", value) <= value <= schema.get("maximum", value):
            fail(f"{value} out of bounds")
    if isinstance(value, list):
        if not schema.get("minItems", 0) <= len(value) <= schema.get("maxItems", len(value)):
            fail("item count out of bounds")
        if schema.get("uniqueItems") and len(set(map(canonical, value))) < len(value):
            fail("duplicate items")
        for index, item in enumerate(value if "items" in schema else ()):
            check(schema["items"], item, root, f"{at}[{index}]")
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        if missing := [name for name in schema.get("required", ()) if name not in value]:
            fail(f"missing {missing}")
        if "additionalProperties" in schema and (extra := sorted(value.keys() - properties.keys())):
            fail(f"unexpected {extra}")
        for name, member in value.items():
            if name in properties:
                check(properties[name], member, root, f"{at}.{name}")


def load(name: str) -> Any:
    return json.loads((API / name).read_text())


class ApiSchemaTests(unittest.TestCase):
    def test_every_schema_has_a_golden_document_that_it_accepts(self) -> None:
        schemas = sorted(API.glob("*.schema.json"))
        self.assertEqual([path.name for path in schemas],
                         ["preflight.schema.json", "probe.schema.json", "servers.schema.json"])
        for path in schemas:
            golden = path.name.replace(".schema.", ".golden.")
            with self.subTest(golden=golden):
                schema = load(path.name)
                require_supported(schema)
                check(schema, load(golden), schema)

    def test_drift_from_a_schema_is_refused(self) -> None:
        def throughput(document: Any) -> Any:
            return document["capabilities"]["throughput"]

        def latency(document: Any) -> Any:
            return document["capabilities"]["latency"][0]

        drifted: list[tuple[str, str, Callable[[Any], object]]] = [
            ("preflight", "unexpected", lambda document: document.update(region="eu")),
            ("preflight", "missing", lambda document: document.pop("lease")),
            ("preflight", "unexpected", lambda document: latency(document).update(weight=2)),
            ("preflight", "not in enum", lambda document: throughput(document)[0].update(protocol="h4")),
            ("preflight", "does not match", lambda document: latency(document).update(baseUrl="ws://a")),
            ("preflight", "not integer", lambda document: document.update(apiVersion=True)),
            ("preflight", "length", lambda document: document.update(generation="")),
            ("preflight", "item count", lambda document: throughput(document).extend(throughput(document) * 7)),
            ("probe", "out of bounds", lambda document: document["load"].update(max=0)),
            ("servers", "duplicate", lambda document: document["defaultSelection"].append("self")),
        ]
        for name, error, mutate in drifted:
            schema, document = load(f"{name}.schema.json"), load(f"{name}.golden.json")
            mutate(document)
            with self.subTest(name=name, error=error), self.assertRaisesRegex(ValueError, error):
                check(schema, document, schema)
        schema = load("preflight.schema.json")
        schema["properties"]["server"]["properties"]["name"]["format"] = "hostname"
        with self.assertRaisesRegex(ValueError, "unsupported schema keywords"):
            require_supported(schema)


if __name__ == "__main__":
    unittest.main()
