"""Synthetic transport-path instrument demo; no retrieval, model or campaign."""
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'eval'))
from runner.l0 import parse_injected_chunks, score_chunks
from runner.score_paths import workspace_chunks

with tempfile.TemporaryDirectory() as directory:
    workspace = Path(directory) / 'fixture'
    workspace.mkdir()
    (workspace / 'app.py').write_text('def authenticate(token):\n    return bool(token)\n')
    native = parse_injected_chunks('--- repos/fixture/app.py:1-2 authenticate ---')
    gold = {'app.py': [(1, 2)]}
    before = score_chunks(native, gold)['file_recall']
    after = score_chunks(workspace_chunks(workspace, native), gold)['file_recall']
    assert before == 0 and after == 1
    print(f'Before (legacy literal paths): file_recall={before}')
    print(f'After (opt-in exact workspace mapping): file_recall={after}')
    try:
        workspace_chunks(workspace, [('repos/foreign/app.py', 1, 2)])
    except ValueError as exc:
        print(f'Foreign-only path: REFUSED ({exc})')
    else:
        raise AssertionError('foreign path accepted')
    assert native == [('repos/fixture/app.py', 1, 2)]
    assert score_chunks(native, gold)['file_recall'] == before
    print('Original literal scoring unchanged; synthetic instrument only, no retrieval/campaign')
