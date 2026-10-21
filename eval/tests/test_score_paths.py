"""Private path instrument controls; no corpus or native/model invocation."""

import os

import pytest

from runner import l0
from runner.score_paths import workspace_chunks


def fixture(tmp_path):
    ws = tmp_path / 'fixture'
    ws.mkdir()
    (ws / 'app.py').write_text('one\ntwo\n')
    return ws


def test_native_qualified_and_literal_paths_share_gold_without_changing_scorer(tmp_path):
    ws = fixture(tmp_path)
    gold = {'app.py': [(1, 2)]}
    native = [('repos/fixture/app.py', 1, 2)]
    assert l0.score_chunks(native, gold)['file_recall'] == 0
    normalized = workspace_chunks(ws, native)
    assert normalized == [('app.py', 1, 2)]
    assert workspace_chunks(ws, normalized) == normalized
    assert l0.score_chunks(normalized, gold)['file_recall'] == 1
    assert native == [('repos/fixture/app.py', 1, 2)]


@pytest.mark.parametrize('path', [
    'repos/other/app.py', 'repos/fixture/missing.py', '../app.py',
    '/app.py', 'repos/fixture/../app.py', 'repos//fixture/app.py',
    'repos/fixture/./app.py', 'repos\\fixture\\app.py', '', 'app.py\0bad',
])
def test_unknown_or_unsafe_path_is_not_a_zero_recall_observation(tmp_path, path):
    with pytest.raises(ValueError):
        workspace_chunks(fixture(tmp_path), [(path, 1, 2)])


def test_ambiguous_qualified_path_refuses_instead_of_choosing_gold(tmp_path):
    ws = fixture(tmp_path)
    other = ws / 'repos/fixture'
    other.mkdir(parents=True)
    (other / 'app.py').write_text('different source\n')
    with pytest.raises(ValueError, match='ambiguous'):
        workspace_chunks(ws, [('repos/fixture/app.py', 1, 2)])
    # A real literal repos/ path stays valid when there is no stripped candidate.
    (ws / 'app.py').unlink()
    assert workspace_chunks(ws, [('repos/fixture/app.py', 1, 2)]) == [
        ('repos/fixture/app.py', 1, 2)]


def test_linked_source_path_refuses(tmp_path):
    ws = fixture(tmp_path)
    (ws / 'link.py').symlink_to(ws / 'app.py')
    with pytest.raises(ValueError, match='linked'):
        workspace_chunks(ws, [('repos/fixture/link.py', 1, 2)])


def test_actual_task_driver_opt_in_maps_hook_paths_legacy_stays_literal(tmp_path, monkeypatch):
    ws = fixture(tmp_path)
    monkeypatch.setattr(l0, 'gold_for_commit', lambda *a: {'app.py': [(1, 2)]})
    monkeypatch.setattr(l0, 'run_hook_arm', lambda *a: (
        'injected', '', [('repos/fixture/app.py', 1, 2)], 80))
    task = {'id': 'fixture', 'commit': 'unused', 'description': 'private'}
    args = (task, ws, ['full'], [100], 'not-executed', {}, lambda *a: None)
    legacy, = l0.run_task(*args)
    actual, = l0.run_task(*args, normalize_paths=True)
    assert legacy.file_recall == 0 and actual.file_recall == 1
    assert actual.injected_tokens == legacy.injected_tokens == 20
    assert actual.chunks == [('app.py', 1, 2)]
    monkeypatch.setattr(l0, 'run_hook_arm', lambda *a: (
        'injected', '', [('repos/foreign/app.py', 1, 2)], 80))
    refused, = l0.run_task(*args, normalize_paths=True)
    assert refused.outcome == 'error' and refused.file_recall is None


def test_native_embed_baseline_normalizes_before_counting_characters(tmp_path, monkeypatch):
    ws = fixture(tmp_path)
    monkeypatch.setattr(l0, 'gold_for_commit', lambda *a: {'app.py': [(1, 2)]})
    monkeypatch.setattr(l0, 'baseline_embed', lambda *a: [('repos/fixture/app.py', 1, 2)])
    task = {'id': 'fixture', 'commit': 'unused', 'description': 'private'}
    score, = l0.run_task(task, ws, ['embed'], [100], 'not-executed', {},
                        lambda *a: None, normalize_paths=True)
    assert score.file_recall == 1 and score.injected_tokens == len('one\ntwo\n') // 4


@pytest.mark.parametrize('fork_name', ['fixture', 'separate-arm'])
def test_no_coupling_checks_its_own_source_not_the_original(tmp_path, monkeypatch, fork_name):
    ws = fixture(tmp_path)
    fork = tmp_path / 'derived' / fork_name
    fork.mkdir(parents=True)
    (fork / 'app.py').write_text('one\ntwo\n')
    monkeypatch.setattr(l0, 'gold_for_commit', lambda *a: {'app.py': [(1, 2)]})
    monkeypatch.setattr(l0, 'run_hook_arm', lambda selected, *a: (
        'injected', '', [(f'repos/{selected.name}/app.py', 1, 2)], 80))
    task = {'id': 'fixture', 'commit': 'unused', 'description': 'private'}
    args = (task, ws, ['full', '-coupling'], [100], 'not-executed', {}, lambda *a: None)
    full, derived = l0.run_task(*args, nocoupling_workspace=fork, normalize_paths=True)
    assert full.file_recall == derived.file_recall == 1
    assert full.injected_tokens == derived.injected_tokens == 20
    (fork / 'app.py').unlink()
    full, missing = l0.run_task(*args, nocoupling_workspace=fork, normalize_paths=True)
    assert full.outcome == 'injected' and full.file_recall == 1
    assert missing.outcome == 'error' and missing.file_recall is None


@pytest.mark.skipif(getattr(os, 'geteuid', lambda: 0)() == 0,
                    reason='POSIX non-root traversal is required to prove permission refusal')
def test_unreadable_alternative_does_not_make_a_path_unique(tmp_path):
    ws = fixture(tmp_path)
    parent = ws / 'repos'
    alternative = parent / 'fixture' / 'app.py'
    alternative.parent.mkdir(parents=True)
    alternative.write_text('other source\n')
    try:
        parent.chmod(0)
        with pytest.raises(ValueError, match='unreadable'):
            workspace_chunks(ws, [('repos/fixture/app.py', 1, 2)])
    finally:
        parent.chmod(0o700)
    with pytest.raises(ValueError, match='ambiguous'):
        workspace_chunks(ws, [('repos/fixture/app.py', 1, 2)])
    alternative.unlink()
    assert workspace_chunks(ws, [('repos/fixture/app.py', 1, 2)]) == [('app.py', 1, 2)]
