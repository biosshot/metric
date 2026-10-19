#!/usr/bin/env python3
"""Verify that every release publisher depends on same-commit reusable CI."""
from pathlib import Path
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[1]


class ReleaseChecks(unittest.TestCase):
    def test_real_symbolication_is_an_unconditional_check(self):
        ci = yaml.load((ROOT / '.github/workflows/ci.yml').read_text(), Loader=yaml.BaseLoader)
        job = ci['jobs']['symbolication']
        self.assertNotIn('if', job)
        self.assertNotIn('continue-on-error', job)
        commands = [step for step in job['steps'] if 'scripts/test-symbolication.py' in step.get('run', '')]
        self.assertEqual(len(commands), 1)
        self.assertNotIn('if', commands[0])
        self.assertNotIn('continue-on-error', commands[0])
        self.assertIn('--backend postgres', commands[0]['run'])

    def test_same_commit_checks_gate_all_publishers(self):
        ci = yaml.load((ROOT / '.github/workflows/ci.yml').read_text(), Loader=yaml.BaseLoader)
        release = yaml.load((ROOT / '.github/workflows/release.yml').read_text(), Loader=yaml.BaseLoader)
        self.assertIn('workflow_call', ci['on'])
        jobs = release['jobs']
        self.assertEqual(jobs['checks']['uses'], './.github/workflows/ci.yml')
        self.assertNotIn('if', jobs['checks'])
        self.assertNotIn('continue-on-error', jobs['checks'])

        def ancestors(name):
            needs = jobs[name].get('needs', [])
            needs = [needs] if isinstance(needs, str) else needs
            return set(needs).union(*(ancestors(parent) for parent in needs))

        for name in ('image', 'helm-chart', 'github-release'):
            with self.subTest(job=name):
                self.assertIn('checks', ancestors(name))
                self.assertNotIn('if', jobs[name])
        for job in ci['jobs'].values():
            for step in job.get('steps', []):
                if step.get('uses', '').startswith('actions/checkout@'):
                    self.assertNotIn('ref', step.get('with', {}))


if __name__ == '__main__':
    unittest.main()
