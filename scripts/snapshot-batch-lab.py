#!/usr/bin/env python3
"""Characterize native document batches against a NEW disposable Quipu only.

There is deliberately no endpoint argument. The child binds loopback, receives
a private synthetic bearer/config/store and is terminated on every exit path.
Existing Quipu can measure the prototype; it cannot pass the transfer contract.
This is not a production publisher or a release of any publication hold.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import urllib.parse

MIB = 1024 * 1024
GIB = 1024 * MIB
CLIENT = 'bobbin-batch-lab'


def save_report(path, report):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as output:
        json.dump(report, output, indent=2)
        output.write('\n')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        raise RuntimeError('lab HTTP redirects are refused')


def cgroup():
    rows = Path('/proc/self/cgroup').read_text().splitlines()
    rel = next(row.split('::', 1)[1] for row in rows if row.startswith('0::'))
    return Path('/sys/fs/cgroup') / rel.lstrip('/')


def memory_limit(root):
    values = []
    for name in ('memory.high', 'memory.max'):
        value = (root / name).read_text().strip()
        if value == 'max':
            raise RuntimeError('finite explicit lab memory.high and memory.max required')
        values.append(int(value))
    if max(values) > 3 * GIB or min(values) <= GIB:
        raise RuntimeError('use an isolated lab scope with finite limits above 1 and at most 3 GiB')
    return min(values) - GIB


def rss(pid):
    fields = {}
    for line in Path(f'/proc/{pid}/status').read_text().splitlines():
        if line.startswith(('VmRSS:', 'VmHWM:')):
            key, value, _unit = line.split()
            fields[key.rstrip(':')] = int(value) * 1024
    return fields['VmRSS'], fields['VmHWM']


class Lab:
    def __init__(self, port, token):
        self.base = f'http://127.0.0.1:{port}'
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect)
        self.calls = []

    def request(self, route, data=None, raw=False, form=False):
        if route not in ('/health', '/version', '/metrics', '/shapes', '/knot/stage',
                         '/knot/promote', '/knot/cancel', '/update'):
            raise RuntimeError('route outside the disposable lab contract')
        request = urllib.request.Request(self.base + route,
            data=None if data is None else (urllib.parse.urlencode(data).encode()
                if form else json.dumps(data).encode()),
            headers={'Content-Type': 'application/x-www-form-urlencoded' if form else 'application/json', 'X-Quipu-Client': CLIENT,
                     'Authorization': 'Bearer ' + self.token})
        self.calls.append(route)
        with self.opener.open(request, timeout=10 if route == '/update' else 0.5) as response:
            payload = response.read(4 * MIB + 1)
        if len(payload) > 4 * MIB:
            raise RuntimeError('oversized lab response')
        return payload.decode() if raw else json.loads(payload)

    def timing(self, path):
        text = self.request('/metrics', raw=True)
        values = {'seconds': 0.0, 'acquisitions': 0.0}
        for line in text.splitlines():
            endpoint = '/knot/promote' if path == 'promote' else '/update'
            if f'endpoint="{endpoint}"' not in line or f'client="{CLIENT}"' not in line:
                continue
            seconds = 'quipu_writer_lock_held_seconds_total{' if path == 'promote' else 'quipu_store_held_seconds_total{'
            for prefix, key in [(seconds, 'seconds'),
                                ('quipu_writer_lock_acquisitions_total{', 'acquisitions')]:
                if line.startswith(prefix):
                    values[key] += float(line.rsplit(' ', 1)[1])
        return values


def digest(data):
    return 'sha256:' + hashlib.sha256(data).hexdigest()


def parts(text):
    data = text.encode()
    while data:
        end = min(len(data), 256 * 1024)
        while True:
            try:
                part = data[:end].decode()
                break
            except UnicodeDecodeError:
                end -= 1
        yield part
        data = data[end:]


def fixture(document, count):
    return {'document': document, 'edges': [], 'chunks': [
        {'id': f'c{i}', 'file_path': document, 'chunk_type': 'function',
         'name': f'fn_{i}_' + '界x' * 20, 'start_line': i + 1, 'end_line': i + 1,
         'content': 'PRIVATE SOURCE BODY', 'language': 'rust', 'tags': ''}
        for i in range(count)]}


def plan(binary, budget, row):
    result = subprocess.run([str(binary), str(budget), '20000', 'fixture',
        'disposable-generation', 'absent-in-fresh-lab'],
        input=json.dumps(row) + '\n', capture_output=True, text=True, timeout=5)
    if result.returncode:
        return None
    batch = json.loads(result.stdout)
    if 'PRIVATE SOURCE BODY' in batch['turtle']:
        raise RuntimeError('native projection leaked source body')
    return batch


def sized_batch(binary, budget, document):
    low, high, accepted = 1, 2048, None
    while low <= high:
        count = (low + high) // 2
        batch = plan(binary, budget, fixture(document, count))
        if batch is None:
            high = count - 1
        else:
            accepted = batch
            low = count + 1
    if accepted is None or len(accepted['turtle'].encode()) < budget * 0.9:
        raise RuntimeError('fixture did not exercise the requested byte ceiling')
    return accepted


def promote(lab, batch, cancel=False, abort=None):
    payloads = list(parts(batch['turtle']))
    upload = digest((batch['snapshot'] + '\n' + batch['content_hash']).encode())
    for number, payload in enumerate(payloads):
        lab.request('/knot/stage', {'upload_id': upload, 'snapshot': batch['snapshot'],
            'content_hash': batch['content_hash'], 'total_parts': len(payloads),
            'total_bytes': len(batch['turtle'].encode()), 'part_number': number,
            'part_hash': digest(payload.encode()), 'payload': payload,
            'actor': 'disposable-batch-fixture', 'source': 'disposable snapshot batch lab'})
    started = time.monotonic()
    result = lab.request('/knot/promote', {'upload_id': upload})
    stop_at, accepted_stop = None, False
    if cancel and result.get('pending'):
        stop_at = time.monotonic()
        stopped = lab.request('/knot/cancel', {'upload_id': upload})
        accepted_stop = stopped.get('cancellation_requested') is True
    while result.get('pending'):
        if abort is not None and abort.is_set() and stop_at is None:
            stop_at = time.monotonic()
            stopped = lab.request('/knot/cancel', {'upload_id': upload})
            accepted_stop = stopped.get('cancellation_requested') is True
        if time.monotonic() - started > 10:
            lab.request('/knot/cancel', {'upload_id': upload})
            raise RuntimeError('lab promotion deadline; outcome remains indeterminate')
        time.sleep(0.01)
        result = lab.request('/knot/promote', {'upload_id': upload})
    cancelled = accepted_stop and result.get('cancelled') is True
    if not cancelled and not (result.get('promoted') is True and result.get('replaced') is True
            and result.get('content_hash') == batch['content_hash'] and 'tx_id' in result
            and result.get('conforms') is not False):
        raise RuntimeError('exact committed or terminal cancelled receipt missing')
    return {'receipt': result, 'wall_seconds': time.monotonic() - started,
            'stop_accepted': accepted_stop, 'cancelled': cancelled,
            'stop_seconds': None if stop_at is None else time.monotonic() - stop_at}


def run(args):
    # No production URL, database, credentials, HOME, or arbitrary child flags.
    root = cgroup()
    cap = memory_limit(root)
    if int((root / 'memory.current').read_text()) >= cap:
        raise RuntimeError('lab baseline already exceeds absolute headroom budget')
    server_binary, planner = args.server.resolve(strict=True), args.planner.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='bobbin-document-lab-') as name:
        home = Path(name)
        (home / '.bobbin').mkdir()
        token = secrets.token_urlsafe(32)
        config = home / '.bobbin/config.toml'
        config.write_text('[quipu]\nbase_ns="urn:bobbin:lab:"\n'
            'code_base="urn:bobbin:lab:code:"\n'
            '[quipu.embedding]\nauto_embed=false\n[quipu.shacl]\n'
            f'validate_on_write={str(args.validation == "on").lower()}\n'
            f'[quipu.server]\nauth_token="{token}"\n')
        config.chmod(0o600)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'RUST_LOG': 'warn',
               'TOKIO_WORKER_THREADS': '2', 'RAYON_NUM_THREADS': '2'}
        log = (home / 'server.log').open('w+')
        server = subprocess.Popen([str(server_binary), '--db', str(home / 'store.db'),
            '--bind', f'127.0.0.1:{port}'], cwd=home, env=env, stdout=log, stderr=log,
            start_new_session=True)
        lab = Lab(port, token)
        try:
            deadline = time.monotonic() + 20
            while True:
                if server.poll() is not None:
                    raise RuntimeError('disposable Quipu exited during startup')
                try:
                    lab.request('/health')
                    break
                except (OSError, urllib.error.URLError):
                    if time.monotonic() >= deadline:
                        raise RuntimeError('disposable Quipu readiness deadline') from None
                    time.sleep(0.1)
            version = lab.request('/version')
            if args.validation == 'on':
                loaded = lab.request('/shapes', {'action': 'load', 'name': 'code-entities',
                    'turtle': args.shapes.read_text()})
                if loaded.get('error') or loaded.get('conforms') is False:
                    raise RuntimeError('disposable shapes did not load')
            results = []
            for kib in args.sizes:
                batch = sized_batch(planner, kib * 1024, f'lab/file-{kib}.rs')
                before = lab.timing(args.path)
                baseline, _ = rss(server.pid)
                sampled = {'rss': baseline, 'cgroup': 0}
                done = threading.Event()
                abort = threading.Event()
                sample_errors = []
                def sample():
                    try:
                        while not done.is_set():
                            current, hwm = rss(server.pid)
                            sampled['rss'] = max(sampled['rss'], current, hwm)
                            sampled['cgroup'] = max(sampled['cgroup'], int((root / 'memory.current').read_text()))
                            if sampled['rss'] - baseline > 256 * MIB or max(sampled.values()) > cap:
                                abort.set()
                            done.wait(0.005)
                    except (OSError, KeyError, ValueError) as error:
                        sample_errors.append(type(error).__name__)
                        abort.set()
                monitor = threading.Thread(target=sample)
                monitor.start()
                seed = None
                try:
                    if args.path == 'promote':
                        if args.existing_snapshot:
                            seed = promote(lab, batch, abort=abort)
                            previous_hash = batch['content_hash']
                            batch['turtle'] = batch['turtle'].replace('rdfs:label "fn_', 'rdfs:label "Fn_')
                            batch['content_hash'] = digest(batch['turtle'].encode())
                            if batch['content_hash'] == previous_hash:
                                raise RuntimeError('existing snapshot control must change labels without new identities')
                            if abort.is_set():
                                raise RuntimeError('seed exhausted resource guard; no replacement admitted')
                            before = lab.timing(args.path)
                        result = promote(lab, batch, abort=abort)
                    else:
                        lines = batch['turtle'].splitlines()
                        prefixes = '\n'.join(line.replace('@prefix ', 'PREFIX ', 1).removesuffix(' .')
                            for line in lines if line.startswith('@prefix '))
                        body = '\n'.join(line for line in lines if not line.startswith('@prefix '))
                        started = time.monotonic()
                        receipt = lab.request('/update', {'update': prefixes + '\nINSERT DATA {\n' + body + '\n}',
                            'actor': 'disposable-batch-fixture', 'source': 'disposable snapshot batch lab'}, form=True)
                        if receipt.get('tx') is None or receipt.get('asserted') != batch['triples'] or receipt.get('retracted') != 0:
                            raise RuntimeError('exact update commit receipt missing')
                        result = {'receipt': receipt, 'wall_seconds': time.monotonic() - started}
                finally:
                    done.set()
                    monitor.join()
                if sample_errors or abort.is_set():
                    raise RuntimeError('lab resource guard or sampler failed; no next batch')
                after = lab.timing(args.path)
                held = after['seconds'] - before['seconds']
                acquisitions = after['acquisitions'] - before['acquisitions']
                if (args.path == 'promote' and acquisitions < 1) or held <= 0:
                    raise RuntimeError('completed isolated writer timing unavailable')
                results.append({'input_ceiling': kib * 1024, 'payload_bytes': len(batch['turtle'].encode()),
                    'triples': batch['triples'], 'content_hash': batch['content_hash'],
                    'writer_held_seconds': held, 'writer_acquisitions': acquisitions if args.path == 'promote' else None,
                    'baseline_rss': baseline, 'peak_rss_upper_bound': sampled['rss'],
                    'delta_rss_upper_bound': sampled['rss'] - baseline,
                    'cgroup_sampled_peak': sampled['cgroup'], 'absolute_budget': cap,
                    'within_observed_memory_targets': sampled['rss'] - baseline <= 256 * MIB
                        and max(sampled['rss'], sampled['cgroup']) <= cap,
                    'within_observed_max_hold': held <= 1,
                    'seed': seed, 'old_source_triples': batch['triples'] if seed else 0, **result})
            stopped = None
            if args.path == 'promote':
                stopped_batch = sized_batch(planner, 64 * 1024, 'lab/stop.rs')
                stopped = promote(lab, stopped_batch, cancel=True)
            # No next batch is submitted after this stop boundary.
            report = {'schema': 'bobbin.document-lab/v1', 'server': version,
                'server_sha256': hashlib.sha256(server_binary.read_bytes()).hexdigest(),
                'planner_sha256': hashlib.sha256(planner.read_bytes()).hexdigest(),
                'store': 'new disposable store; auto_embed=false',
                'validation': args.validation, 'path': args.path,
                'existing_snapshot': args.existing_snapshot,
                'existing_snapshot_control': 'seed native payload then change only label case with same bytes/triples/identities/key; memory bound includes seed',
                'validation_off_caveat': 'off disables config validation and loads no shapes; vocabulary bootstrap is inactive',
                'timing_method': 'isolated serial client/endpoint completed writer counter deltas',
                'memory_method': '5ms RSS/cgroup samples plus process lifetime VmHWM (conservative)',
                'batches': results, 'stop': stopped,
                'phase_observations': [json.loads(line) for line in (home / 'server.log').read_text().splitlines()
                    if line.startswith('{') and '"update_observation"' in line],
                'qualified': False,
                'unmet_gates': ['bounded CAS legacy ownership transfer unavailable',
                    'incremental-reader contract not implemented', 'representative copied-store/adversarial matrix',
                    'valid concurrent /episode and /update probe coverage',
                    'server-enforced per-batch deadlines/cardinality and exact scoped peak evidence'],
                'publication_authority': 'LAB ONLY; no production or config authority'}
            save_report(args.output, report)
            print(json.dumps({'report': str(args.output), 'qualified': False,
                'bytes': [r['payload_bytes'] for r in results],
                'held_seconds': [r['writer_held_seconds'] for r in results]}))
            return report
        except Exception as error:
            log.flush()
            log.seek(0)
            diagnostic = log.read()[-4000:].replace(token, '[REDACTED]')
            save_report(args.output, {'schema': 'bobbin.document-lab/v1', 'qualified': False,
                'outcome': 'UNKNOWN_OR_FAILED', 'error_type': type(error).__name__,
                'error': str(error)[:1000], 'server_diagnostic': diagnostic,
                'production_effects': False})
            raise RuntimeError(f'disposable lab failed; evidence: {args.output}') from None
        finally:
            if server.poll() is None:
                os.killpg(server.pid, signal.SIGTERM)
                try:
                    server.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(server.pid, signal.SIGKILL)
                    server.wait(timeout=3)
            log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', required=True, type=Path)
    parser.add_argument('--planner', required=True, type=Path)
    parser.add_argument('--shapes', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--characterize-existing', action='store_true',
        help='record an explicitly unqualified baseline; never an acceptance result')
    parser.add_argument('--sizes', type=int, nargs='+', default=[64, 128, 256, 512])
    parser.add_argument('--validation', choices=('on', 'off'), default='on',
        help='off is a private-store characterization control, never a production setting')
    parser.add_argument('--path', choices=('promote', 'update'), default='promote')
    parser.add_argument('--existing-snapshot', action='store_true',
        help='seed each exact producer key then replace its labels, preserving identities and byte count')
    args = parser.parse_args()
    if args.output.exists():
        parser.error('output exists; preserve prior evidence and choose a new report path')
    if args.existing_snapshot and args.path != 'promote':
        parser.error('existing snapshot control requires the promote path')
    if not args.sizes or len(args.sizes) > 4 or len(set(args.sizes)) != len(args.sizes) or any(s not in (64, 128, 256, 512) for s in args.sizes):
        parser.error('sizes must be a nonempty unique selection from 64, 128, 256, 512 KiB')
    report = run(args)
    if not report['qualified'] and not args.characterize_existing:
        raise SystemExit(3)


if __name__ == '__main__':
    main()
