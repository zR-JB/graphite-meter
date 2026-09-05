from __future__ import annotations

import json
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from scripts.legal.artifacts import render, review_audit, review_template
from scripts.legal.discovery import (
    discover_browser, discover_go, go_discovery_targets, go_packages, go_toolchain_component,
    has_go_replacement_provenance, legal_go_version, package_license, package_root,
    repository_go_toolchain_version, repository_url, source_for,
)
from scripts.legal.model import Component, Json, LegalError, LegalFile, LocalArtifact, Project, Provenance, Review, read_json, sha256
from scripts.legal.review import (
    add_provenance, component_from_files, component_legal_files, prepare_scopes,
    read_legal_files, refresh_reviewed_versions, reviewed_legal_files, sort_components, validate_review,
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
        file = LegalFile('LICENSE', sha256(b'MIT License\n'), 'MIT License\n', 'license')
        return (
            Component('example', '1', 'npm', declaredLicenseExpression='MIT', selectedLicenseExpression='MIT', legalTexts=[file]),
            Review('npm', 'example', '1', declaredLicenseExpression='MIT', selectedLicenseExpression='MIT',
                   legalFiles=[replace(file, text='')], reviewDecision='approved'),
        )

    def test_legal_json_rejects_duplicate_fields_and_nonfinite_values(self) -> None:
        for value in ('{"reviewDecision":"pending","reviewDecision":"approved"}', '{"version":NaN}'):
            path = self.write('review.json', value)
            with self.subTest(value=value), self.assertRaises(LegalError):
                read_json(path)

    def test_legal_files_preserve_bytes_kinds_and_nested_population(self) -> None:
        contents = 'MIT License\r\n<script>literal</script>\n'
        self.write('license.MIT', contents)
        self.write('NOTICE.txt', 'Copyright upstream\n')
        self.write('README.md', 'not legal')
        self.write('THIRD_PARTY_LICENSES', 'embedded licenses')
        self.write('vendor/foo/LICENSE', 'nested license')
        files = read_legal_files(self.root)
        self.assertEqual([item.name for item in files], ['license.MIT', 'NOTICE.txt', 'THIRD_PARTY_LICENSES', 'vendor/foo/LICENSE'])
        self.assertEqual(files[0].text, contents)
        self.assertEqual(files[0].sha256, sha256(contents.encode()))
        self.assertEqual([item.kind for item in files], ['license', 'notice', 'notice', 'license'])
        self.assertEqual(len(read_legal_files(self.root, recursive=False)), 3)

    def test_missing_legal_material_fails(self) -> None:
        self.write('README.md', 'no license')
        with self.assertRaisesRegex(LegalError, 'no legal candidate'):
            read_legal_files(self.root)

    def test_approval_reuses_version_only_changes_but_requires_complete_fingerprint(self) -> None:
        component, review = self.reviewed()
        validate_review(component, [review])
        validate_review(replace(component, version='2'), [review])
        changed = replace(component, legalTexts=[replace(component.legalTexts[0], sha256=sha256(b'changed'))])
        with self.assertRaisesRegex(LegalError, 'fingerprint') as caught:
            validate_review(changed, [review])
        for value in ('LICENSE', component.legalTexts[0].sha256, sha256(b'changed')):
            self.assertIn(value, str(caught.exception))
        for changed in (
            replace(component, legalTexts=[]),
            replace(component, notices=[LegalFile('NOTICE', sha256(b'new notice'))]),
            replace(component, modified=True),
            replace(component, declaredLicenseExpression='MIT OR GPL-3.0'),
        ):
            with self.subTest(component=changed), self.assertRaises(LegalError):
                validate_review(changed, [review])

    def test_approval_rejects_unreviewed_unknown_and_unresolved_material(self) -> None:
        component, review = self.reviewed()
        with self.assertRaisesRegex(LegalError, 'new component'):
            validate_review(component, [])
        for expression in ('', 'UNKNOWN', 'NOASSERTION', 'UNLICENSED'):
            with self.subTest(expression=expression), self.assertRaisesRegex(LegalError, 'licensing information'):
                validate_review(replace(component, declaredLicenseExpression=expression), [review])
        with self.assertRaisesRegex(LegalError, 'unresolved'):
            validate_review(component, [replace(review, reviewDecision='pending')])
        with self.assertRaisesRegex(LegalError, 'declared license'):
            validate_review(component, [replace(review, selectedLicenseExpression='')])

    def test_reviewed_nested_files_are_rehashed_and_new_candidates_require_review(self) -> None:
        root_text, nested_text = 'MIT License\nroot\n', 'MIT License\nnested\n'
        self.write('LICENSE', root_text)
        nested = self.write('vendor/foo/LICENSE', nested_text)
        review = Review('npm', 'example', declaredLicenseExpression='MIT', selectedLicenseExpression='MIT', reviewDecision='approved',
                        legalFiles=[LegalFile('LICENSE', sha256(root_text.encode())), LegalFile('vendor/foo/LICENSE', sha256(nested_text.encode()))])
        files = component_legal_files(self.root, 'npm', 'example', [review])
        validate_review(component_from_files('npm', 'example', '1', '', files), [review])
        nested.write_text('changed nested license')
        files = component_legal_files(self.root, 'npm', 'example', [review])
        self.assertEqual(files[1].sha256, sha256(b'changed nested license'))
        with self.assertRaisesRegex(LegalError, 'fingerprint'):
            validate_review(component_from_files('npm', 'example', '1', '', files), [review])
        self.write('NOTICE', 'unreviewed notice')
        with self.assertRaisesRegex(LegalError, 'new legal file.*NOTICE'):
            component_legal_files(self.root, 'npm', 'example', [review])

    def test_reviewed_files_resolve_isolated_bun_siblings_and_reject_traversal(self) -> None:
        self.write('svelte/package.json', '{}')
        self.write('magic-string/LICENSE', 'dependency license')
        review = Review('npm', 'svelte', legalFiles=[LegalFile('node_modules/magic-string/LICENSE')])
        files = component_legal_files(self.root / 'svelte', 'npm', 'svelte', [review])
        self.assertEqual(files[0].name, 'node_modules/magic-string/LICENSE')
        self.assertEqual(files[0].text, 'dependency license')
        for name in ('../LICENSE', '/absolute/LICENSE', '.'):
            with self.subTest(name=name), self.assertRaisesRegex(LegalError, 'inside component'):
                reviewed_legal_files(self.root, 'npm', 'svelte', [replace(review, legalFiles=[LegalFile(name)])])

    def test_inference_separates_notices_without_approving_a_license(self) -> None:
        for content, expected in (
            ('MIT License\n', 'MIT'), ('Permission is hereby granted, free of charge', 'MIT'),
            ('Permission to use, copy, modify, and distribute', 'ISC'), ('ISC License', 'ISC'),
            ('BSD 3-Clause\nRedistribution and use in source and binary forms', 'BSD-3-Clause'),
            ('BSD 2-Clause', 'BSD-2-Clause'), ('Apache License Version 2.0', 'Apache-2.0'),
        ):
            with self.subTest(content=content):
                component = component_from_files('go', 'example', '1', '', [LegalFile('LICENSE', text=content, kind='license'), LegalFile('NOTICE', text='notice', kind='notice')])
                self.assertEqual(component.selectedLicenseExpression, expected)
                self.assertEqual(len(component.legalTexts), 1)
                self.assertEqual(len(component.notices), 1)
                with self.assertRaisesRegex(LegalError, 'new component'):
                    validate_review(component, [])

    def test_scoped_review_selection_and_version_refresh(self) -> None:
        component, review = self.reviewed()
        scopes = {'tui': [replace(component, version='2', selectedLicenseExpression='MIT OR GPL-3.0')]}
        unused = replace(review, name='unused', reviewedVersion='7')
        prepare_scopes(scopes, [review], 'check')
        self.assertEqual(scopes['tui'][0].selectedLicenseExpression, 'MIT')
        refresh_reviewed_versions(scopes, [review, unused])
        self.assertEqual((review.reviewedVersion, unused.reviewedVersion), ('2', '7'))
        unresolved = {'tui': [Component('new', ecosystem='npm')]}
        prepare_scopes(unresolved, [], 'review-template')
        with self.assertRaisesRegex(LegalError, 'tui:'):
            prepare_scopes(unresolved, [], 'generate')

    def test_provenance_checks_artifact_bytes_and_flows_into_container(self) -> None:
        asset = self.write('font.woff2', 'font bytes')
        license_path = self.write('manual/LICENSE', 'MIT License\n')
        entry = Provenance('font', 'Example Font', '1', licenseExpression='MIT', artifactScopes=['server/browser'], reviewNotes='reviewed',
                           localArtifacts=[LocalArtifact('font.woff2', sha256(asset.read_bytes()))],
                           localLegalFiles=[LegalFile('manual/LICENSE', sha256(license_path.read_bytes()))])
        server = add_provenance(self.root, [], [entry], 'server/browser')
        container = add_provenance(self.root, server, [entry], 'container')
        self.assertEqual(container, server)
        self.assertEqual(container[0].legalTexts[0].name, 'LICENSE')
        asset.write_text('changed bytes')
        with self.assertRaisesRegex(LegalError, 'local artifact hash changed'):
            add_provenance(self.root, [], [entry], 'server/browser')
        asset.write_text('font bytes')
        license_path.write_text('changed license')
        with self.assertRaisesRegex(LegalError, 'legal file hash changed'):
            add_provenance(self.root, [], [entry], 'server/browser')

    def test_browser_scan_uses_nested_package_roots_and_actual_bundled_ids(self) -> None:
        root = self.root / 'node_modules/@scope/package'
        self.write('node_modules/@scope/package/package.json', '{"name":"@scope/package","version":"1","license":"MIT","repository":{"url":"git+https://example.invalid/package.git"}}')
        self.write('node_modules/@scope/package/LICENSE', 'MIT License\n')
        module = root / 'dist/index.js'
        self.assertEqual(package_root(str(module)), root)
        scan = self.write('scan.json', json.dumps([str(module), str(module), '/app/src/main.ts']))
        components = discover_browser(scan, [])
        self.assertEqual(len(components), 1)
        self.assertEqual((components[0].name, components[0].source), ('@scope/package', 'https://example.invalid/package'))
        self.assertEqual(package_license(None, [{'type': 'ISC'}], 'UNKNOWN'), 'ISC')
        self.assertEqual(repository_url(None, 'golang.org/x/net'), 'https://go.googlesource.com/net')
        self.assertEqual(source_for('github.com/example/module'), 'https://github.com/example/module')
        self.assertEqual(source_for('anything', 'explicit'), 'explicit')

    def test_go_targets_and_replacements_preserve_scope(self) -> None:
        targets = go_discovery_targets(ROOT)
        self.assertEqual([f'{os}/{arch}' for scope, os, arch in targets if scope == 'server'], ['linux/amd64', 'linux/arm64'])
        self.assertEqual([f'{os}/{arch}' for scope, os, arch in targets if scope == 'tui'], (ROOT / 'scripts/tui-targets.txt').read_text().splitlines())
        entries = [Provenance('go', 'replacement', 'v1', artifactScopes=['server/browser'])]
        self.assertFalse(has_go_replacement_provenance([], 'replacement', 'v1', 'server'))
        self.assertTrue(has_go_replacement_provenance(entries, 'replacement', 'v1', 'server'))
        self.assertTrue(has_go_replacement_provenance(entries, 'replacement', '', 'server'))
        self.assertFalse(has_go_replacement_provenance(entries, 'replacement', 'v2', 'server'))
        self.assertFalse(has_go_replacement_provenance(entries, 'replacement', 'v1', 'tui'))
        for content in ('', 'linux/amd64\nlinux/amd64\n', 'linux/amd64/extra\n', 'linux/amd64\n'):
            self.write('scripts/tui-targets.txt', content)
            with self.subTest(content=content), self.assertRaises(LegalError):
                go_discovery_targets(self.root)

    def test_go_discovery_uses_toolchain_metadata_and_resolved_replacement(self) -> None:
        self.write('LICENSE', 'MIT License\n')
        packages: list[dict[str, Json]] = [
            {'Standard': True}, {'Module': {'Path': 'github.com/zR-JB/graphite-meter/go'}},
            {'Module': {'Path': 'example/module', 'Replace': {'Path': '../replacement', 'Dir': str(self.root)}}},
        ]
        provenance = [Provenance('go', '../replacement', 'local', artifactScopes=['server'])]
        with patch('scripts.legal.discovery.go_discovery_targets', return_value=[('server', 'linux', 'arm64')]), \
             patch('scripts.legal.discovery.go_packages', return_value=packages) as listing, \
             patch('scripts.legal.discovery.go_toolchain_component', return_value=Component('Go standard library', ecosystem='go-toolchain')):
            with self.assertRaisesRegex(LegalError, 'requires provenance'):
                discover_go(self.root, [], [])
            server, tui = discover_go(self.root, [], provenance)
            listing.assert_called_with(self.root, './cmd/graphite-meter', 'linux', 'arm64')
            self.assertEqual(server[0].source_path, self.root)
            self.assertEqual(server[0].name, '../replacement')
            self.assertEqual(tui[0].ecosystem, 'go-toolchain')
        with patch('scripts.legal.discovery.run_go', return_value=' {"ImportPath":"one"}\n {"Standard":true}\n') as command:
            self.assertEqual(len(go_packages(self.root, './cmd/server', 'linux', 'arm64')), 2)
            command.assert_called_once_with(self.root, 'list', '-deps', '-json', './cmd/server', target=('linux', 'arm64'))

    def test_go_pin_and_canonical_toolchain_legal_material(self) -> None:
        for raw, expected in (('go1.27.1', 'go1.27.1'), ('go1.27.1-X:experiment', 'go1.27.1'),
                              ('go1.27.1 local build', 'go1.27.1'), ('devel go1.28-abcd', 'devel go1.28-abcd')):
            self.assertEqual(legal_go_version(raw), expected)
        for content in ('go 1.27\n', 'go latest\n', 'go 1.27.1\ntoolchain go1.27.1 extra\n'):
            self.write('go/go.mod', content)
            with self.assertRaises(LegalError):
                repository_go_toolchain_version(self.root)
        self.write('go/go.mod', 'module example\ngo 1.27.0\ntoolchain go1.27.1\n')
        self.write('legal/toolchains/go/LICENSE', 'Redistribution and use in source and binary forms')
        self.write('legal/toolchains/go/PATENTS', 'canonical snapshot')
        with patch('scripts.legal.discovery.run_go', return_value='go1.27.1 local build\n') as command:
            component = go_toolchain_component(self.root)
            self.assertEqual(component.version, 'go1.27.1')
            self.assertEqual(component.notices[0].text, 'canonical snapshot')
            command.assert_called_once_with(self.root, 'env', 'GOVERSION')
        with patch('scripts.legal.discovery.run_go', return_value='go1.27.0\n'), self.assertRaisesRegex(LegalError, 'pins go1.27.1.*go1.27.0'):
            go_toolchain_component(self.root)

    def test_rendering_preserves_scope_release_identity_and_private_source_path(self) -> None:
        self.write('LICENSE', 'Project license')
        project = Project(name='Graphite Meter', repository='https://example.invalid/repo')
        component, review = self.reviewed()
        component.source_path = self.root
        scopes = {'server/browser': [component], 'tui': [component], 'container': [component]}
        for version, source in (('v1.2.3-rc.4', project.repository + '/tree/v1.2.3-rc.4'), ('development', project.repository), ('0.0.0-dev+abc', project.repository)):
            files = render(self.root, project, version, scopes)
            self.assertEqual(files['legal/generated/tui/SOURCE.txt'], (source + '\n').encode())
            self.assertIn(b'Project license\n\nTHIRD-PARTY', files['go/internal/legal/assets/TUI_LEGAL.txt'])
            self.assertNotIn(str(self.root).encode(), files['legal/generated/server/inventory.json'])
        self.assertIn(b'fingerprint-matches', review_audit(scopes, [review]))
        self.assertIn(b'"selectedLicenseExpression": ""', review_template(scopes))
        self.assertEqual([item.name for item in sort_components([replace(component, name='b'), component, replace(component, source='updated')])], ['b', 'example'])


if __name__ == '__main__':
    unittest.main()
