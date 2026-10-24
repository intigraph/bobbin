#!/usr/bin/env python3
"""Install release-shaped fixtures; failures must preserve an existing command."""
from contextlib import redirect_stderr, redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from urllib.request import urlopen as real_urlopen
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
CONFIG = {'archive': 'bobbin-{tag}-{target}.tar.gz', 'checksum': 'SHA256SUMS.txt', 'directory': '{archive}', 'commands': ['bobbin'], 'targets': {'Linux:x86_64': 'x86_64-unknown-linux-gnu', 'Linux:aarch64': 'aarch64-unknown-linux-gnu', 'Darwin:x86_64': 'x86_64-apple-darwin', 'Darwin:arm64': 'aarch64-apple-darwin'}, 'repo': 'bobbin'}
TAG = 'v0.29.0'
VERSION = '0.29.0'
TARGET = CONFIG['targets']['Linux:x86_64']


class ReleaseInstall(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='release-install-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.release = self.root / 'release'
        self.release.mkdir()
        self.prefix = self.root / 'prefix'
        (self.prefix / 'bin').mkdir(parents=True)
        self.existing = self.prefix / 'bin' / CONFIG['commands'][0]
        self.existing.write_text('previous installed bytes')
        self.env = dict(os.environ, HOME=str(self.root / 'home'), STACK_VERSION=TAG,
                        STACK_INSTALL_ROOT=str(self.prefix), STACK_RELEASE_BASE_URL=self.release.as_uri())
        self.name = CONFIG['archive'].format(tag=TAG, version=VERSION, target=TARGET)
        self.archive = self.release / self.name
        self.fixture()
        self.latest = {"tag_name": TAG, "draft": False, "prerelease": False}

    def fixture(self, version=VERSION, extra=None):
        directory = CONFIG['directory'].format(archive=self.name.removesuffix('.tar.gz'))
        with tarfile.open(self.archive, 'w:gz') as bundle:
            for command in CONFIG['commands']:
                body = f'#!/bin/sh\necho "{command} {version}"\n'.encode()
                member = tarfile.TarInfo(str(Path(directory) / command))
                member.size, member.mode = len(body), 0o755
                bundle.addfile(member, io.BytesIO(body))
            if extra:
                member = tarfile.TarInfo(extra)
                member.size = 1
                bundle.addfile(member, io.BytesIO(b'x'))
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        manifest = CONFIG['checksum'].format(archive=self.name)
        (self.release / manifest).write_text(f'{digest}  {self.name}\n')

    def open_release(self, request, **kwargs):
        if request.full_url.startswith('https://api.github.com/repos/scbrown/'):
            return io.BytesIO(json.dumps(self.latest).encode())
        return real_urlopen(request, **kwargs)

    def run_installer(self, system='Linux', machine='x86_64'):
        # Execute this exact entrypoint's embedded Python with only platform
        # detection substituted; fixture requests use real file:// downloads.
        script = (ROOT / 'install.sh').read_text().split("<<'PYINSTALL'\n", 1)[1].rsplit('PYINSTALL', 1)[0]
        stdout, stderr = io.StringIO(), io.StringIO()
        status = 0
        with patch.dict(os.environ, self.env, clear=True), patch('sys.argv', ['install.sh']), patch('urllib.request.urlopen', side_effect=self.open_release), \
             patch('platform.system', return_value=system), patch('platform.machine', return_value=machine), \
             redirect_stdout(stdout), redirect_stderr(stderr):
            try:
                exec(compile(script, str(ROOT / 'install.sh'), 'exec'), {'__name__': '__main__'})
            except SystemExit as error:
                status = error.code
        return status, stdout.getvalue(), stderr.getvalue()

    def assert_preserved(self, result):
        self.assertNotEqual(result[0], 0, result)
        self.assertEqual(self.existing.read_text(), 'previous installed bytes')
        self.assertFalse(self.existing.is_symlink())
        self.assertNotIn('Installed verified', result[1])

    def test_install_verified_release_and_run_linked_command(self):
        result = self.run_installer()
        self.assertEqual(result[0], 0, result)
        self.assertTrue(self.existing.is_symlink())
        output = subprocess.check_output([str(self.existing), '--version'], text=True)
        self.assertIn(VERSION, output)

    def test_latest_release_selection_installs_the_resolved_tag(self):
        self.env.pop('STACK_VERSION')
        result = self.run_installer()
        self.assertEqual(result[0], 0, result)
        self.assertIn(TAG, result[1])

    def test_prerelease_cannot_become_latest_stable_install(self):
        self.env.pop('STACK_VERSION')
        self.latest['prerelease'] = True
        self.assert_preserved(self.run_installer())

    def test_reinstall_of_intact_release_succeeds(self):
        self.assertEqual(self.run_installer()[0], 0)
        result = self.run_installer()
        self.assertEqual(result[0], 0, result)

    def test_tampered_existing_bundle_cannot_claim_verified_success(self):
        self.assertEqual(self.run_installer()[0], 0)
        self.existing.resolve().write_text('tampered installed bytes')
        result = self.run_installer()
        self.assertNotEqual(result[0], 0, result)
        self.assertIn('installed bundle bytes differ', result[2])
        self.assertNotIn('Installed verified', result[1])
        self.assertEqual(self.existing.read_text(), 'tampered installed bytes')

    def test_corrupt_checksum_preserves_previous(self):
        self.archive.write_bytes(self.archive.read_bytes() + b'corruption')
        self.assert_preserved(self.run_installer())

    def test_missing_archive_preserves_previous(self):
        self.archive.unlink()
        self.assert_preserved(self.run_installer())

    def test_unsupported_platform_preserves_previous(self):
        self.assert_preserved(self.run_installer(system='UnsupportedOS'))

    def test_wrong_version_preserves_previous(self):
        self.fixture(version='999.999.999')
        self.assert_preserved(self.run_installer())

    def test_archive_traversal_preserves_previous(self):
        self.fixture(extra='../escaped')
        self.assert_preserved(self.run_installer())
        self.assertFalse((self.root / 'escaped').exists())

    def test_ambiguous_checksum_preserves_previous(self):
        manifest = self.release / CONFIG['checksum'].format(archive=self.name)
        manifest.write_text(manifest.read_text() * 2)
        self.assert_preserved(self.run_installer())


if __name__ == '__main__':
    unittest.main(verbosity=2)
