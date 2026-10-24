#!/bin/sh
# Install verified published bytes; never build the invoking checkout.
set -eu
exec python3 - "$@" <<'PYINSTALL'
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

CONFIG = {'archive': 'bobbin-{tag}-{target}.tar.gz', 'checksum': 'SHA256SUMS.txt', 'directory': '{archive}', 'commands': ['bobbin'], 'targets': {'Linux:x86_64': 'x86_64-unknown-linux-gnu', 'Linux:aarch64': 'aarch64-unknown-linux-gnu', 'Darwin:x86_64': 'x86_64-apple-darwin', 'Darwin:arm64': 'aarch64-apple-darwin'}, 'repo': 'bobbin'}


def fetch(url, destination):
    for attempt in range(3):
        try:
            with urlopen(Request(url, headers={'User-Agent': CONFIG['repo'] + '-installer'}), timeout=60) as response:
                with destination.open('wb') as output:
                    shutil.copyfileobj(response, output)
            return
        except (HTTPError, URLError, TimeoutError) as error:
            transient = not isinstance(error, HTTPError) or error.code in (403, 429, 500, 502, 503, 504)
            if attempt == 2 or not transient:
                raise
            time.sleep(attempt + 1)


def extract(archive, destination):
    with tarfile.open(archive, 'r:gz') as bundle:
        seen, links = set(), []
        total = 0
        for member in bundle:
            name = PurePosixPath(member.name)
            if name.is_absolute() or '..' in name.parts or str(name) in seen:
                raise ValueError('unsafe or duplicate archive path')
            seen.add(str(name))
            target = destination.joinpath(*name.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            if member.isdir():
                target.mkdir(exist_ok=True)
            elif member.isfile():
                total += member.size
                if total > 1024 * 1024 * 1024:
                    raise ValueError('release exceeds 1 GiB unpacked limit')
                source = bundle.extractfile(member)
                if source is None:
                    raise ValueError('missing archive bytes')
                with source, target.open('xb') as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
            elif member.issym():
                link = PurePosixPath(member.linkname)
                if link.is_absolute() or '..' in link.parts:
                    raise ValueError('unsafe archive link')
                links.append((target, member.linkname))
            else:
                raise ValueError('unsupported archive member')
        for target, link in links:
            target.symlink_to(link)
            target.resolve(strict=True).relative_to(destination.resolve())


def bundle_identity(root):
    result = {}
    for path in root.rglob('*'):
        relative = str(path.relative_to(root))
        if relative == '.release.json':
            continue
        if path.is_symlink():
            result[relative] = ('link', os.readlink(path))
        elif path.is_file():
            result[relative] = ('file', path.stat().st_mode & 0o777,
                                hashlib.sha256(path.read_bytes()).hexdigest())
        elif path.is_dir():
            result[relative] = ('directory',)
        else:
            raise ValueError('unexpected installed bundle entry')
    return result


def install():
    if len(sys.argv) > 1:
        raise ValueError('configure STACK_INSTALL_ROOT and STACK_VERSION through the environment; no positional arguments')
    system, machine = platform.system(), platform.machine()
    target = CONFIG['targets'].get(system + ':' + machine)
    if target is None:
        raise ValueError('no published archive for ' + system + ' ' + machine)
    with tempfile.TemporaryDirectory(prefix=CONFIG['repo'] + '-release-') as temporary:
        work = Path(temporary)
        tag = os.environ.get('STACK_VERSION')
        if not tag:
            metadata = work / 'latest.json'
            fetch('https://api.github.com/repos/scbrown/' + CONFIG['repo'] + '/releases/latest', metadata)
            release = json.loads(metadata.read_text())
            if release.get('draft') or release.get('prerelease'):
                raise ValueError('latest release is not stable')
            tag = release['tag_name']
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,100}', tag):
            raise ValueError('invalid release tag')
        match = re.search(r'(?:^|v)([0-9]+\.[0-9]+\.[0-9]+)$', tag)
        if not match:
            raise ValueError('unsupported release version')
        version = match.group(1)
        name = CONFIG['archive'].format(tag=tag, version=version, target=target)
        checksum = CONFIG['checksum'].format(archive=name)
        base = os.environ.get('STACK_RELEASE_BASE_URL', 'https://github.com/scbrown/' + CONFIG['repo'] + '/releases/download/' + tag)
        archive, manifest = work / name, work / 'checksums'
        fetch(base + '/' + name, archive)
        fetch(base + '/' + checksum, manifest)
        matches = []
        for line in manifest.read_text().splitlines():
            fields = line.split()
            if len(fields) == 2 and fields[1].lstrip('*') == name:
                matches.append(fields[0].lower())
        if len(matches) != 1 or not re.fullmatch('[0-9a-f]{64}', matches[0]):
            raise ValueError('missing or ambiguous checksum entry for ' + name)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != matches[0]:
            raise ValueError('release checksum mismatch')
        unpacked = work / 'unpacked'
        unpacked.mkdir()
        extract(archive, unpacked)
        bundle = unpacked / CONFIG['directory'].format(archive=name.removesuffix('.tar.gz'))
        for command in CONFIG['commands']:
            executable = bundle / command
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise ValueError('release lacks executable ' + command)
        output = subprocess.check_output([str(bundle / CONFIG['commands'][0])] + CONFIG.get('version_args', ['--version']), text=True, timeout=20)
        if not re.search(r'(?<![0-9.])' + re.escape(version) + r'(?![0-9.])', output):
            raise ValueError('release version mismatch: ' + output.strip())
        prefix = Path(os.environ.get('STACK_INSTALL_ROOT', str(Path.home() / '.local'))).expanduser().absolute()
        binaries, releases = prefix / 'bin', prefix / 'lib' / CONFIG['repo']
        binaries.mkdir(parents=True, exist_ok=True)
        releases.mkdir(parents=True, exist_ok=True)
        lock = releases / '.install-lock'
        lock.mkdir()  # refuse concurrent mutation; never remove another installer's lock
        try:
            destination = releases / tag
            for command in CONFIG['commands']:
                if (binaries / command).is_dir():
                    raise ValueError('install destination is a directory: ' + command)
            identity = {'archive': name, 'sha256': digest, 'tag': tag}
            if destination.is_symlink():
                raise ValueError('release destination is a symlink')
            if destination.exists():
                if json.loads((destination / '.release.json').read_text()) != identity:
                    raise ValueError('existing release identity differs; previous install preserved')
                if bundle_identity(destination) != bundle_identity(bundle):
                    raise ValueError('installed bundle bytes differ; previous command links preserved')
            else:
                with tempfile.TemporaryDirectory(prefix='.candidate-', dir=releases) as candidate:
                    candidate_bundle = Path(candidate) / 'bundle'
                    shutil.copytree(bundle, candidate_bundle, symlinks=True)
                    (candidate_bundle / '.release.json').write_text(json.dumps(identity) + '\n')
                    candidate_bundle.rename(destination)
            for command in CONFIG['commands']:
                fd, staged = tempfile.mkstemp(prefix='.' + command + '-', dir=binaries)
                os.close(fd)
                os.unlink(staged)
                try:
                    os.symlink(destination / command, staged)
                    os.replace(staged, binaries / command)
                finally:
                    if os.path.lexists(staged):
                        os.unlink(staged)
            print(output.strip())
            print('Installed verified ' + tag + ' in ' + str(destination))
            print('Add ' + str(binaries) + ' to PATH. Previous release directories are retained for rollback.')
        finally:
            lock.rmdir()


try:
    install()
except (OSError, ValueError, KeyError, subprocess.SubprocessError, tarfile.TarError) as error:
    print('Install refused: ' + str(error), file=sys.stderr)
    sys.exit(1)
PYINSTALL
