#!/usr/bin/env bash
# arming: agent-invoked -- isolated CLI/SessionStart reproduction, no live service.
set -eu
DEMO_BIN=${BOBBIN_DEMO_BIN:-bobbin}
DEMO_ROOT=$(mktemp -d)
trap 'rm -rf "$DEMO_ROOT"' EXIT
unset BOBBIN_SERVER
"$DEMO_BIN" --version
"$DEMO_BIN" init "$DEMO_ROOT" > /dev/null
printf '%s\n' '$ bobbin mark auth-refresh "token refresh flow" "Authentication refresh context"'
"$DEMO_BIN" mark auth-refresh 'token refresh flow' 'Authentication refresh context' --path "$DEMO_ROOT"
printf '%s\n' '$ bobbin hook prime-context < SessionStart.json'
(cd "$DEMO_ROOT" && printf '%s\n' '{"source":"startup","session_id":"breadcrumb-demo"}' | "$DEMO_BIN" hook prime-context) > "$DEMO_ROOT/response.json"
python3 - "$DEMO_ROOT/response.json" <<'PY'
import json, sys
response = json.load(open(sys.argv[1]))['hookSpecificOutput']
context = response['additionalContext']
print('hookEventName=' + response['hookEventName'])
print('ordinary_primer=' + str('Index Status' in context).lower())
if '=== Breadcrumbs ===' in context:
    print(context[context.index('=== Breadcrumbs ==='):].rstrip())
else:
    print('breadcrumb discovery: absent')
PY
