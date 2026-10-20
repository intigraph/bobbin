#!/usr/bin/env python3
"""Bounded hosted readback; build policy and credentials stay on trusted host."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import time

spec = importlib.util.spec_from_file_location('publisher', Path(__file__).with_name('publish_repository_share.py'))
publisher = importlib.util.module_from_spec(spec); spec.loader.exec_module(publisher)


def verify(folder, tag, revision):
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:[.-][A-Za-z0-9.-]+)?', tag) or not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('invalid release identity')
    archive = folder / ('bobbin-' + tag + '-repository.pendant.tar.gz')
    sha = hashlib.sha256(archive.read_bytes()).hexdigest()
    if archive.with_name(archive.name + '.sha256').read_text() != sha + '  ' + archive.name + '\n':
        raise ValueError('archive checksum mismatch')
    proof = json.loads(archive.with_name(archive.name + '.provenance.json').read_text())
    expected = {'schema': 'quipu.repository-share-producer/v1', 'source_repository': 'scbrown/bobbin',
                'source_revision': revision, 'release_tag': tag, 'archive_sha256': sha, 'destination': 'outward'}
    if any(proof.get(k) != v for k, v in expected.items()):
        raise ValueError('repository/source/tag/provenance mismatch')
    for key in ('quipu_sha256', 'quipu_server_sha256', 'bobbin_sha256'):
        if not re.fullmatch('[0-9a-f]{64}', proof.get(key, '')):
            raise ValueError('missing engine/indexer digest')
    if not re.fullmatch('[0-9a-f]{40}', proof.get('engine_source_revision', '')):
        raise ValueError('missing engine source pin')
    with tempfile.TemporaryDirectory(prefix='verify-pendant-') as temp:
        stage = Path(temp)
        with tarfile.open(archive) as tar:
            members = tar.getmembers()
            if (len(members) != len({m.name for m in members})
                    or any(not m.isfile() or m.name not in publisher.MEMBERS for m in members)):
                raise ValueError('unexpected, duplicate, or unsafe archive member')
            # Every member is a flat allowlisted regular file; no path, link or device extraction.
            for member in members:
                (stage / member.name).write_bytes(tar.extractfile(member).read())
        publisher.check_share(stage)
    return expected


def wait(tag, revision, attempts=80, pause=time.sleep, run=subprocess.run):
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:[.-][A-Za-z0-9.-]+)?', tag) or not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('invalid release identity before lookup')
    stem = 'bobbin-' + tag + '-repository.pendant.tar.gz'
    last_read = 'not attempted'
    for number in range(attempts):
        with tempfile.TemporaryDirectory(prefix='repository-pendant-readback-') as temp:
            folder = Path(temp)
            try:
                result = run(['gh', 'release', 'download', tag, '--repo', 'scbrown/bobbin', '--dir', temp,
                              '--pattern', stem, '--pattern', stem + '.sha256', '--pattern', stem + '.provenance.json'],
                             capture_output=True, text=True, timeout=45)
                last_read = 'exit=' + str(result.returncode)
                if result.returncode == 0:
                    return verify(folder, tag, revision)
            except subprocess.TimeoutExpired:
                last_read = 'read timeout'
        if number + 1 < attempts:
            pause(30)
    raise ValueError('repository pendant unverified after bounded trusted-producer wait; ' + last_read)


if __name__ == '__main__':
    if len(sys.argv) != 3:
        raise SystemExit('usage: verify-repository-share.py <release-tag> <exact-release-sha>')
    print(json.dumps(wait(sys.argv[1], sys.argv[2])))
