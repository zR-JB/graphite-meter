from __future__ import annotations

import json
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

from scripts.legal.artifacts import about, copyright_notice
from scripts.legal.discovery import discover_browser, package_license, package_root, repository_url
from scripts.legal.model import (
    Component, LegalError, LegalFile, LocalArtifact, Project, Provenance, Review, array, read_json, sha256,
)
from scripts.legal.review import (
    add_provenance, component_from_files, component_legal_files, prepare_scopes,
    read_legal_files, reviewed_legal_files, validate_review,
)

ROOT = Path(__file__).resolve().parents[2]


class LegalTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, name: str, content: str) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content.encode())
        return path

    def reviewed(self) -> tuple[Component, Review]:
        file = LegalFile("LICENSE", sha256(b"MIT License\n"), "MIT License\n", "license")
        return (
            Component("example", "1", "npm", declaredLicenseExpression="MIT",
                      selectedLicenseExpression="MIT", legalTexts=[file]),
            Review("npm", "example", "1", declaredLicenseExpression="MIT", selectedLicenseExpression="MIT",
                   legalFiles=[replace(file, text="")], reviewDecision="approved"),
        )

    def test_legal_json_rejects_duplicate_fields_and_nonfinite_values(self) -> None:
        for value in ('{"reviewDecision":"pending","reviewDecision":"approved"}', '{"version":NaN}'):
            path = self.write("review.json", value)
            with self.subTest(value=value), self.assertRaises(LegalError):
                read_json(path)

    def test_legal_files_preserve_bytes_kinds_and_nested_population(self) -> None:
        contents = "MIT License\r\n<script>literal</script>\n"
        self.write("license.MIT", contents)
        self.write("NOTICE.txt", "Copyright upstream\n")
        self.write("README.md", "not legal")
        self.write("THIRD_PARTY_LICENSES", "embedded licenses")
        self.write("vendor/foo/LICENSE", "nested license")
        files = read_legal_files(self.root)
        self.assertEqual([item.name for item in files],
                         ["license.MIT", "NOTICE.txt", "THIRD_PARTY_LICENSES", "vendor/foo/LICENSE"])
        self.assertEqual(files[0].text, contents)
        self.assertEqual(files[0].sha256, sha256(contents.encode()))
        self.assertEqual([item.kind for item in files], ["license", "notice", "notice", "license"])
        self.assertEqual(len(read_legal_files(self.root, recursive=False)), 3)

    def test_missing_legal_material_fails(self) -> None:
        self.write("README.md", "no license")
        with self.assertRaisesRegex(LegalError, "no legal candidate"):
            read_legal_files(self.root)

    def test_approval_reuses_version_only_changes_but_requires_complete_fingerprint(self) -> None:
        component, review = self.reviewed()
        validate_review(component, [review])
        validate_review(replace(component, version="2"), [review])
        changed = replace(component, legalTexts=[replace(component.legalTexts[0], sha256=sha256(b"changed"))])
        with self.assertRaisesRegex(LegalError, "fingerprint") as caught:
            validate_review(changed, [review])
        for value in ("LICENSE", component.legalTexts[0].sha256, sha256(b"changed")):
            self.assertIn(value, str(caught.exception))
        for changed in (
            replace(component, legalTexts=[]),
            replace(component, notices=[LegalFile("NOTICE", sha256(b"new notice"))]),
            replace(component, modified=True),
            replace(component, declaredLicenseExpression="MIT OR GPL-3.0"),
        ):
            with self.subTest(component=changed), self.assertRaises(LegalError):
                validate_review(changed, [review])

    def test_approval_rejects_unreviewed_unknown_and_unresolved_material(self) -> None:
        component, review = self.reviewed()
        with self.assertRaisesRegex(LegalError, "new component"):
            validate_review(component, [])
        for expression in ("", "UNKNOWN", "NOASSERTION", "UNLICENSED"):
            with self.subTest(expression=expression), self.assertRaisesRegex(LegalError, "licensing information"):
                validate_review(replace(component, declaredLicenseExpression=expression), [review])
        with self.assertRaisesRegex(LegalError, "unresolved"):
            validate_review(component, [replace(review, reviewDecision="pending")])
        with self.assertRaisesRegex(LegalError, "declared license"):
            validate_review(component, [replace(review, selectedLicenseExpression="")])

    def test_reviewed_nested_files_are_rehashed_and_new_candidates_require_review(self) -> None:
        root_text, nested_text = "MIT License\nroot\n", "MIT License\nnested\n"
        self.write("LICENSE", root_text)
        nested = self.write("vendor/foo/LICENSE", nested_text)
        legal = [LegalFile("LICENSE", sha256(root_text.encode())),
                 LegalFile("vendor/foo/LICENSE", sha256(nested_text.encode()))]
        review = Review("npm", "example", declaredLicenseExpression="MIT", selectedLicenseExpression="MIT",
                        reviewDecision="approved", legalFiles=legal)
        files = component_legal_files(self.root, "npm", "example", [review])
        validate_review(component_from_files("npm", "example", "1", "", files), [review])
        nested.write_text("changed nested license")
        files = component_legal_files(self.root, "npm", "example", [review])
        self.assertEqual(files[1].sha256, sha256(b"changed nested license"))
        with self.assertRaisesRegex(LegalError, "fingerprint"):
            validate_review(component_from_files("npm", "example", "1", "", files), [review])
        self.write("NOTICE", "unreviewed notice")
        with self.assertRaisesRegex(LegalError, "new legal file.*NOTICE"):
            component_legal_files(self.root, "npm", "example", [review])

    def test_reviewed_files_resolve_isolated_bun_siblings_and_reject_traversal(self) -> None:
        self.write("patched-lib/package.json", "{}")
        self.write("sibling-lib/LICENSE", "dependency license")
        review = Review("npm", "patched-lib", legalFiles=[LegalFile("node_modules/sibling-lib/LICENSE")])
        files = component_legal_files(self.root / "patched-lib", "npm", "patched-lib", [review])
        self.assertEqual(files[0].name, "node_modules/sibling-lib/LICENSE")
        self.assertEqual(files[0].text, "dependency license")
        for name in ("../LICENSE", "/absolute/LICENSE", "."):
            with self.subTest(name=name), self.assertRaisesRegex(LegalError, "inside component"):
                reviewed_legal_files(self.root, "npm", "patched-lib", [replace(review, legalFiles=[LegalFile(name)])])

    def test_inference_separates_notices_without_approving_a_license(self) -> None:
        for content, expected in (
            ("MIT License\n", "MIT"), ("Permission is hereby granted, free of charge", "MIT"),
            ("Permission to use, copy, modify, and distribute", "ISC"), ("ISC License", "ISC"),
            ("BSD 3-Clause\nRedistribution and use in source and binary forms", "BSD-3-Clause"),
            ("BSD 2-Clause", "BSD-2-Clause"), ("Apache License Version 2.0", "Apache-2.0"),
        ):
            with self.subTest(content=content):
                legal = [LegalFile("LICENSE", text=content, kind="license"),
                         LegalFile("NOTICE", text="notice", kind="notice")]
                component = component_from_files("cargo", "example", "1", "", legal)
                self.assertEqual(component.selectedLicenseExpression, expected)
                self.assertEqual(len(component.legalTexts), 1)
                self.assertEqual(len(component.notices), 1)
                with self.assertRaisesRegex(LegalError, "new component"):
                    validate_review(component, [])

    def test_scoped_review_selection(self) -> None:
        component, review = self.reviewed()
        scopes = {"tui": [replace(component, version="2", selectedLicenseExpression="MIT OR GPL-3.0")]}
        prepare_scopes(scopes, [review], "check")
        self.assertEqual(scopes["tui"][0].selectedLicenseExpression, "MIT")
        unresolved = {"tui": [Component("new", ecosystem="npm")]}
        prepare_scopes(unresolved, [], "review-template")
        with self.assertRaisesRegex(LegalError, "tui:"):
            prepare_scopes(unresolved, [], "generate")

    def test_provenance_checks_artifact_bytes_and_flows_into_container(self) -> None:
        asset = self.write("font.woff2", "font bytes")
        license_path = self.write("manual/LICENSE", "MIT License\n")
        entry = Provenance("font", "Example Font", "1", licenseExpression="MIT",
                           artifactScopes=["server/browser"], reviewNotes="reviewed",
                           localArtifacts=[LocalArtifact("font.woff2", sha256(asset.read_bytes()))],
                           localLegalFiles=[LegalFile("manual/LICENSE", sha256(license_path.read_bytes()))])
        server = add_provenance(self.root, [], [entry], "server/browser")
        container = add_provenance(self.root, server, [entry], "container")
        self.assertEqual(container, server)
        self.assertEqual(container[0].legalTexts[0].name, "LICENSE")
        asset.write_text("changed bytes")
        with self.assertRaisesRegex(LegalError, "local artifact hash changed"):
            add_provenance(self.root, [], [entry], "server/browser")
        asset.write_text("font bytes")
        license_path.write_text("changed license")
        with self.assertRaisesRegex(LegalError, "legal file hash changed"):
            add_provenance(self.root, [], [entry], "server/browser")

    def test_browser_scan_uses_nested_package_roots_and_actual_bundled_ids(self) -> None:
        root = self.root / "node_modules/@scope/package"
        metadata = {"name": "@scope/package", "version": "1", "license": "MIT",
                    "repository": {"url": "git+https://example.invalid/package.git"}}
        self.write("node_modules/@scope/package/package.json", json.dumps(metadata))
        self.write("node_modules/@scope/package/LICENSE", "MIT License\n")
        module = root / "dist/index.js"
        self.assertEqual(package_root(str(module)), root)
        scan = self.write("scan.json", json.dumps([str(module), str(module), "/app/src/main.ts"]))
        components = discover_browser(scan, [])
        self.assertEqual(len(components), 1)
        self.assertEqual((components[0].name, components[0].source),
                         ("@scope/package", "https://example.invalid/package"))
        self.assertEqual(package_license(None, [{"type": "ISC"}], "UNKNOWN"), "ISC")
        self.assertEqual(repository_url(None), "")

    def test_a_browser_scan_names_each_bundled_package_once(self) -> None:
        modules = []
        for name in ("second", "first"):
            metadata = {"name": name, "version": "1", "license": "MIT"}
            self.write(f"node_modules/{name}/package.json", json.dumps(metadata))
            self.write(f"node_modules/{name}/LICENSE", "MIT License\n")
            modules.append(str(self.root / f"node_modules/{name}/index.js"))
        scan = self.write("scan.json", json.dumps([*modules, modules[0], "/app/client/src/main.ts"]))
        self.assertEqual([item.name for item in discover_browser(scan, [])], ["first", "second"])

    def test_browser_scan_identifies_exact_patched_packages_in_isolated_installs(self) -> None:
        self.write("client/package.json", json.dumps({
            "patchedDependencies": {"patched-lib@5.57.1": "patches/patched-lib.patch"}}))
        modules = []
        for name, version in (("patched-lib", "5.57.1"), ("plain-lib", "1.2.2")):
            root = f"client/node_modules/.bun/{name}@{version}/node_modules/{name}"
            self.write(f"{root}/package.json", json.dumps({"name": name, "version": version, "license": "MIT"}))
            self.write(f"{root}/LICENSE", "MIT License\n")
            modules.append(str(self.root / root / "index.js"))
        scan = self.write("scan.json", json.dumps(modules))
        components = {item.name: item for item in discover_browser(scan, [])}
        self.assertTrue(components["patched-lib"].modified)
        self.assertFalse(components["plain-lib"].modified)
        self.write("client/package.json", json.dumps({"patchedDependencies": {"patched-lib@5.57.0": "old.patch"}}))
        self.assertFalse(next(item for item in discover_browser(scan, []) if item.name == "patched-lib").modified)

    def test_the_committed_fonts_and_ca_roots_match_their_reviews(self) -> None:
        reviews = [Review.parse(item) for item in array(read_json(ROOT / "legal/reviewed-components.json"))]
        entries = [Provenance.parse(item) for item in array(read_json(ROOT / "legal/provenance.json"))]
        prepare_scopes({scope: add_provenance(ROOT, [], entries, scope) for scope in ("server/browser", "container")},
                       reviews, "check")

    def test_copyright_follows_the_project_record(self) -> None:
        self.assertEqual((ROOT / "COPYRIGHT").read_text(encoding="utf-8"), copyright_notice(Project.read(ROOT)))

    def test_about_links_each_component_to_its_source_and_a_fork_or_patch_to_its_upstream_and_changes(self) -> None:
        fork = "https://github.com/owner/quic"
        self.write("legal/rust-forks.json", json.dumps([{"fork": fork, "rev": "f" * 40,
                                                         "upstream": "https://github.com/upstream/quic",
                                                         "base": "b" * 40}]))
        patches = {"patched-lib@5.0.0": "patches/patched-lib@5.0.0.patch"}
        self.write("client/package.json", json.dumps({"patchedDependencies": patches}))
        components = [
            Component("ring", "0.17.14", "cargo", "registry+https://github.com/rust-lang/crates.io-index"),
            Component("quic", "1.3.0", "cargo", f"git+{fork}?rev={'f' * 40}#{'f' * 40}", modified=True),
            Component("patched-lib", "5.0.0", "npm", "https://github.com/example/patched-lib", modified=True),
            Component("local", "1", "npm", "file:vendor/local"),
        ]
        project = Project(name="Graphite Meter", repository="https://github.com/owner/meter")
        for source_url, blob in ((project.repository, project.repository + "/blob/HEAD"),
                                 (project.repository + "/tree/v1.2.3", project.repository + "/blob/v1.2.3")):
            listed = json.loads(about(self.root, project, "1.2.3", source_url, components))["components"]
            self.assertEqual({item["name"]: item["links"] for item in listed}, {
                "ring": [{"label": "Source", "url": "https://crates.io/crates/ring/0.17.14"}],
                "quic": [{"label": "Source", "url": f"{fork}/tree/{'f' * 40}"},
                         {"label": "Upstream", "url": f"https://github.com/upstream/quic/tree/{'b' * 40}"},
                         {"label": "Changes", "url": f"{fork}/compare/{'b' * 40}...{'f' * 40}"}],
                "patched-lib": [{"label": "Source", "url": "https://github.com/example/patched-lib"},
                           {"label": "Changes", "url": f"{blob}/client/patches/patched-lib%405.0.0.patch"}],
                "local": [],
            })
            # The review identity stays as reviewed.
            self.assertEqual(listed[0]["source"], "registry+https://github.com/rust-lang/crates.io-index")


if __name__ == "__main__":
    unittest.main()
