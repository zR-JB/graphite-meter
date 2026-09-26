from __future__ import annotations

import unittest
from copy import deepcopy

from scripts.legal.model import LegalError
from scripts.legal.rust import artifacts


class RustArtifactTests(unittest.TestCase):
    def messages(self) -> list[dict]:
        return [
            {'reason': 'compiler-artifact', 'package_id': 'dependency',
             'target': {'name': 'dependency', 'kind': ['lib']},
             'profile': {'test': False}, 'features': ['b', 'a'],
             'fresh': True, 'executable': None},
            {'reason': 'build-script-executed', 'package_id': 'dependency',
             'linked_libs': ['static=crypto']},
            {'reason': 'compiler-artifact', 'package_id': 'application',
             'target': {'name': 'application', 'kind': ['bin']},
             'profile': {'test': False}, 'features': [],
             'fresh': True, 'executable': '/target/application'},
            {'reason': 'build-finished', 'success': True},
        ]

    def test_cached_build_preserves_features_and_native_libraries(self) -> None:
        result = artifacts(self.messages(), 'application', 'application')
        self.assertEqual(result['dependency']['units'][0]['features'], ['a', 'b'])
        self.assertEqual(result['dependency']['nativeLibraries'], ['static=crypto'])
        self.assertEqual(set(result), {'application', 'dependency'})

    def test_failed_truncated_or_wrong_binary_build_is_rejected(self) -> None:
        messages = self.messages()
        for bad in (messages[:-1], messages[:-1] + [{'reason': 'build-finished', 'success': False}], messages[:2] + messages[-1:]):
            with self.subTest(messages=bad), self.assertRaises(LegalError):
                artifacts(bad, 'application', 'application')

    def test_tests_and_examples_cannot_expand_release_closure(self) -> None:
        for kind in ('test', 'example', 'bench'):
            messages = deepcopy(self.messages())
            messages[0]['target']['kind'] = [kind]
            with self.subTest(kind=kind), self.assertRaises(LegalError):
                artifacts(messages, 'application', 'application')


if __name__ == '__main__':
    unittest.main()
