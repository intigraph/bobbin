"""Resolve frozen native context paths without guessing another repository.

Only the opt-in, identity-sealed L0 runner uses this mapping. Legacy rows and
literal scoring remain unchanged. An ambiguous or unknown path is an error,
never a zero-recall observation.
"""
import stat
from pathlib import Path, PurePosixPath


def workspace_chunks(workspace: Path, chunks: list[tuple[str, int, int]]) -> list[tuple[str, int, int]]:
    """Map literal or exact repos/<workspace-name>/ paths to existing files."""
    root = workspace.resolve(strict=True)
    prefix = f"repos/{workspace.name}/"
    result = []
    for path, start, end in chunks:
        if (not isinstance(path, str) or not path or "\\" in path or "\0" in path
                or PurePosixPath(path).is_absolute() or ".." in PurePosixPath(path).parts
                or PurePosixPath(path).as_posix() != path):
            raise ValueError("unsafe native chunk path")
        names = {path}
        if path.startswith(prefix):
            names.add(path[len(prefix):])
        candidates = set()
        for name in names:
            if not name:
                continue
            target = root
            metadata = None
            for part in PurePosixPath(name).parts:
                target = target / part
                try:
                    metadata = target.lstat()
                except (FileNotFoundError, NotADirectoryError):
                    metadata = None
                    break
                except OSError as exc:
                    raise ValueError("unreadable native chunk path") from exc
                if stat.S_ISLNK(metadata.st_mode):
                    raise ValueError("linked native chunk path")
            if metadata is not None and stat.S_ISREG(metadata.st_mode):
                candidates.add(name)
        if len(candidates) != 1:
            raise ValueError("unknown or ambiguous native chunk path")
        result.append((candidates.pop(), start, end))
    return result
