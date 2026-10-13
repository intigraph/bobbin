#!/usr/bin/env bash
# deploy-from-release.sh — PULL-based deploy: fetch a published GitHub release
# artifact, verify it, and hand it to deploy-cutover.sh (aegis-56a).
#
# WHY PULL AND NOT A BUILD RUNNER. This bead was originally scoped as "stand up a
# self-hosted GitHub Actions runner". That design was retired on 2026-08-03: a pull
# needs no inbound path, no runner registration, and no build toolchain on the
# serving host (which is a thermally marginal low-TDP mobile CPU that throttles
# under sustained load). The
# release artifacts are already built by CI on four targets; re-building them on the
# serving host buys nothing and costs a runner nobody maintains.
#
# WHY THE TARBALL AND NOT `bobbin-linux-amd64`. The x86_64 Linux matrix leg
# publishes its tarball plus a matching early SHA256SUMS.txt as soon as that leg
# finishes, before the slower macOS builds.  The finalizer later replaces the
# manifest with one covering every final asset, including the raw binaries.  The
# tarball is therefore the one artifact that is checksummed in BOTH states: while
# the release is still assembling and after it is complete.  Pulling it lets the
# homelab deploy safely without waiting on platforms it never runs (aegis-if8dp).
#
# THE CHAIN OF CUSTODY, end to end:
#   1. download <tarball> + SHA256SUMS.txt from the named release
#   2. VERIFY the tarball against SHA256SUMS.txt   -> refuse on mismatch
#   3. extract, and check the binary reports the version we asked for
#   4. FEATURE PRE-FLIGHT: the same featureless-sentinel probe deploy-cutover.sh
#      uses, run HERE so a bad artifact is caught before anything reaches the host
#   5. hand to deploy-cutover.sh, which owns the dangerous half: stage (never over
#      live), `--version` ABI gate ON THE HOST, feature gate, snapshot -> PREV,
#      atomic swap, restart, smoke, and ROLLBACK on any smoke failure
#
# Step 4 duplicates a check deploy-cutover.sh already makes, deliberately: the same
# probe is cheap, and catching a featureless artifact locally means never having
# staged it on the serving host at all. The gate downstream stays authoritative.
#
# Usage:  deploy-from-release.sh [<tag>]          (default: latest release)
# Env:    DEPLOY_HOST (required, passed through), DRY_RUN=1 to stop after step 4,
#         TARGET (default x86_64-unknown-linux-gnu), REQUIRE_KNOWLEDGE (see cutover).
#         BOBBIN_VERIFIED_TARBALL + BOBBIN_VERIFIED_SHA256: consumer-verified local
#         artifact; both required together, copied and hashed before extraction.
set -euo pipefail

TAG="${1:-}"
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
REPO="${REPO:-scbrown/bobbin}"
: "${DEPLOY_HOST:?set DEPLOY_HOST (the serving host)}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ -z "$TAG" ]; then
  TAG=$(gh release view --repo "$REPO" --json tagName -q .tagName)
fi
echo "==> release $TAG  (repo $REPO, target $TARGET)"

tarball="bobbin-$TAG-$TARGET.tar.gz"

# --- 1. fetch ---------------------------------------------------------------
# Consumer-owned provenance verification may supply the exact verified bytes.
# bobbin-preverified-artifact/1
if [ -n "${BOBBIN_VERIFIED_TARBALL:-}${BOBBIN_VERIFIED_SHA256:-}" ]; then
  [[ "${BOBBIN_VERIFIED_SHA256:-}" =~ ^[0-9a-f]{64}$ ]] &&
    [ -f "${BOBBIN_VERIFIED_TARBALL:-}" ] || {
      echo 'REFUSED: incomplete preverified artifact input' >&2; exit 1;
    }
  cp -- "$BOBBIN_VERIFIED_TARBALL" "$work/$tarball"
  printf '%s  %s\n' "$BOBBIN_VERIFIED_SHA256" "$tarball" > "$work/SHA256SUMS.txt"
else
  gh release download "$TAG" --repo "$REPO" -p "$tarball" -p 'SHA256SUMS.txt' -D "$work" --clobber
fi

# --- 2. VERIFY — refuse anything the published checksums do not cover --------
# `sha256sum -c` on a filtered list: a tarball absent from SHA256SUMS.txt yields an
# EMPTY filter and `-c` then reports "no properly formatted checksum lines", which
# is a FAILURE here rather than a pass. That is the case this whole script exists to
# avoid, so it must not degrade to a warning.
# Compare the filename field exactly: signature/SBOM sidecars share its prefix.
if ! awk -v asset="$tarball" '$2 == asset { print; found=1 } END { exit !found }' \
    "$work/SHA256SUMS.txt" > "$work/want.sha256"; then
  echo "REFUSED: $tarball has no entry in SHA256SUMS.txt — nothing to verify against" >&2
  exit 1
fi
( cd "$work" && sha256sum -c want.sha256 ) || { echo "REFUSED: checksum mismatch on $tarball" >&2; exit 1; }
echo "==> checksum OK"

# --- 3. extract + confirm it is the version we asked for ---------------------
tar -xzf "$work/$tarball" -C "$work"
bin="$(find "$work" -type f -name bobbin -perm -u+x | head -1)"
[ -n "$bin" ] || { echo "REFUSED: no bobbin binary in $tarball" >&2; exit 1; }
chmod +x "$bin"
got="$("$bin" --version 2>&1 | head -1)"
echo "==> artifact reports: $got"
case "$got" in
  *"${TAG#v}"*) ;;
  *) echo "REFUSED: artifact reports '$got' but the release tag is $TAG" >&2; exit 1 ;;
esac

# --- 4. FEATURE PRE-FLIGHT (same probe deploy-cutover.sh uses) ---------------
# The sentinel is compiled in ONLY under cfg(not(feature="knowledge")), so its
# PRESENCE proves a featureless build. Checked here so such an artifact never
# reaches the serving host; deploy-cutover.sh checks again and stays authoritative.
if [ "${REQUIRE_KNOWLEDGE:-1}" = 1 ]; then
  if grep -qa "Knowledge graph tools require the 'knowledge' feature" "$bin"; then
    echo "REFUSED: release artifact was built WITHOUT --features knowledge (featureless sentinel present)." >&2
    echo "         Deploying it would silently break knowledge_query/knowledge_context fleet-wide." >&2
    exit 1
  fi
  echo "==> feature pre-flight OK: knowledge-enabled"
fi

if [ "${DRY_RUN:-0}" = 1 ]; then
  echo "==> DRY_RUN=1: verified and stopping before cutover. Artifact at $bin"
  cp "$bin" "${DRY_RUN_OUT:-/tmp/bobbin-verified}"
  echo "==> copy kept at ${DRY_RUN_OUT:-/tmp/bobbin-verified}"
  exit 0
fi

# --- 5. hand to the cutover, which owns the dangerous half -------------------
# Not `exec`: exec replaces this shell, so the EXIT trap never ran and every
# deploy left its ~250 MB work dir behind in a RAM-backed /tmp (aegis-86f2v7.1).
# Under `set -e` a failed cutover still exits with its status; the trap cleans up.
echo "==> handing to deploy-cutover.sh (host $DEPLOY_HOST)"
"$here/deploy-cutover.sh" "$bin"
