"""Verify source packaging and the standalone PAWS entry point."""
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import scripts.package_paws as packaging


class PackagingTests(unittest.TestCase):
    def test_archive_is_reproducible_source_only_and_collect_needs_no_native_module(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            first, second = root / 'first.tar.gz', root / 'second.tar.gz'
            packaging.package_sources(first)
            packaging.package_sources(second)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with tarfile.open(first) as archive:
                names = archive.getnames()
                self.assertEqual(sorted(name for name in names if name.endswith('.html')), [
                    'web/connections.html', 'web/creators.html',
                    'web/deorphanize.html', 'web/index.html',
                ])
                self.assertIn('web/src/app.ts', names)
                self.assertIn('scripts/build-web.mjs', names)
                self.assertIn('src/lib.rs', names)
                self.assertFalse(any(name.startswith((
                    'data/', 'web/data/', 'web/assets/', 'node_modules/', 'target/', '.venv/',
                )) for name in names))
                self.assertFalse(any(name.endswith(('.so', '.wasm', '.pyc')) for name in names))
                archive.extractall(root / 'project', filter='data')
            probe = """import sys
import orphanwiki.__main__ as cli
sys.argv = ['orphanwiki', 'collect', '--help']
try:
    cli.main()
except SystemExit as result:
    assert result.code == 0
assert 'orphanwiki._native' not in sys.modules
"""
            result = subprocess.run([sys.executable, '-c', probe], cwd=root / 'project',
                                    capture_output=True, text=True, check=True)
            self.assertIn('--low-memory', result.stdout)

    def test_preparation_failure_preserves_existing_archive(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'paws.tar.gz'
            output.write_bytes(b'existing package')
            with patch.object(packaging.tarfile.TarFile, 'addfile', side_effect=OSError('disk full')):
                with self.assertRaisesRegex(OSError, 'disk full'):
                    packaging.package_sources(output)
            self.assertEqual(output.read_bytes(), b'existing package')
            self.assertFalse(output.with_suffix('.gz.tmp').exists())
