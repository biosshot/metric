#!/usr/bin/env python3
"""Verify the POSIX wizard driver with real PowerShell console input, without Docker."""
import os
from pathlib import Path
import runpy
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(os.name == 'posix', 'exercises the POSIX PTY driver')
class WizardTerminal(unittest.TestCase):
    def test_real_read_host_round_trips_three_answers(self):
        powershell = shutil.which('pwsh')
        self.assertIsNotNone(powershell, 'PowerShell is required for this gate')
        driver = runpy.run_path(str(ROOT / 'scripts/test-installer.py'), run_name='terminal_test')
        with tempfile.TemporaryDirectory(prefix='metric-terminal-') as scratch:
            script = Path(scratch) / 'prompts.ps1'
            script.write_text(
                "$ErrorActionPreference = 'Stop'\n"
                "$first = Read-Host 'First answer'\n"
                "$second = Read-Host 'Second answer'\n"
                "$third = Read-Host 'Third answer'\n"
                "if ($first -cne 'mongodb' -or $second -cne 'bundled' -or $third -cne 'medium') { throw 'Incorrect terminal input' }\n"
                "Write-Host 'Terminal input verified'\n",
                encoding='utf-8',
            )
            env = dict(os.environ, TERM='xterm-256color')
            literal_path = str(script).replace("'", "''")
            for arguments in (
                ['-File', str(script)],
                ['-Command', f"Get-Content -Raw -LiteralPath '{literal_path}' | Invoke-Expression"],
            ):
                with self.subTest(invocation=arguments[0]):
                    output = driver['wizard_run'](
                        [powershell, '-NoProfile', *arguments], env,
                        [('First answer', 'mongodb'), ('Second answer', 'bundled'), ('Third answer', 'medium')],
                    )
                    self.assertIn('Terminal input verified', output)


if __name__ == '__main__':
    unittest.main()
