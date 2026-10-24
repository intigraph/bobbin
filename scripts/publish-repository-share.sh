#!/usr/bin/env bash
# Repo-owned job, invoked only by the managed trusted-host release dispatcher.
set -euo pipefail
umask 077
exec python3 "$(dirname -- "${BASH_SOURCE[0]}")/publish_repository_share.py" "$@"
