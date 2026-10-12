#!/usr/bin/env bash
# arming: ci .github/workflows/ci.yml
# Exercise the actual pull-deploy script without network or a serving host.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/release" "$scratch/payload"
cat > "$scratch/payload/bobbin" <<'BIN'
#!/usr/bin/env bash
echo 'bobbin 0.20.1'
BIN
chmod +x "$scratch/payload/bobbin"
asset=bobbin-v0.20.1-x86_64-unknown-linux-gnu.tar.gz
tar czf "$scratch/release/$asset" -C "$scratch/payload" bobbin
sum="$(sha256sum "$scratch/release/$asset")"; sum="${sum%% *}"
cat > "$scratch/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${BLOCK_DOWNLOAD:-0}" == 1 ]]; then
  echo 'unexpected download' > "$DOWNLOAD_MARKER"; exit 99
fi
[[ "$1 $2" == 'release download' ]]
while (($#)); do
  if [[ "$1" == -D ]]; then dest="$2"; shift; fi
  shift
done
cp "$FIXTURE_RELEASE/"* "$dest/"
GH
chmod +x "$scratch/bin/gh"
for mode in plain siblings missing corrupt; do
  manifest="$scratch/release/SHA256SUMS.txt"
  printf '%s  %s.sigstore.json\n%s  %s.sbom.sigstore.json\n' "$sum" "$asset" "$sum" "$asset" > "$manifest"
  case "$mode" in
    plain) printf '%s  %s\n' "$sum" "$asset" > "$manifest" ;;
    siblings) printf '%s  %s\n' "$sum" "$asset" >> "$manifest" ;;
    missing) : ;;
    corrupt) printf '%064d  %s\n' 0 "$asset" >> "$manifest" ;;
  esac
  out="$scratch/verified-$mode"
  rc=0
  env PATH="$scratch/bin:/usr/bin:/bin" FIXTURE_RELEASE="$scratch/release" \
    DEPLOY_HOST=serving.test DRY_RUN=1 DRY_RUN_OUT="$out" \
    bash "$root/scripts/deploy-from-release.sh" v0.20.1 > "$scratch/log" 2>&1 || rc=$?
  case "$mode" in
    plain|siblings)
      if [[ "$rc" != 0 ]] || ! cmp -s "$out" "$scratch/payload/bobbin"; then
        cat "$scratch/log"; echo "FAIL: $mode"; exit 1
      fi ;;
    missing|corrupt)
      if [[ "$rc" == 0 || -e "$out" ]]; then
        cat "$scratch/log"; echo "FAIL: $mode was accepted"; exit 1
      fi
      grep -q 'REFUSED:' "$scratch/log"
      if [[ "$mode" == missing ]]; then grep -q 'has no entry' "$scratch/log"; fi ;;
  esac
  echo "PASS: $mode"
done

# Consumer custody: valid local bytes work with the network downloader disabled.
# A wrong digest, missing file or either half of the contract must never deploy.
for mode in valid wrong-digest missing-file missing-digest missing-path; do
  verified_path="$scratch/release/$asset"; verified_sum="$sum"
  case "$mode" in
    wrong-digest) verified_sum=$(printf '%064d' 0) ;;
    missing-file) verified_path="$scratch/absent" ;;
    missing-digest) verified_sum='' ;;
    missing-path) verified_path='' ;;
  esac
  out="$scratch/local-$mode"; rc=0
  env PATH="$scratch/bin:/usr/bin:/bin" BLOCK_DOWNLOAD=1 DOWNLOAD_MARKER="$scratch/downloaded" \
    BOBBIN_VERIFIED_TARBALL="$verified_path" BOBBIN_VERIFIED_SHA256="$verified_sum" \
    DEPLOY_HOST=serving.test DRY_RUN=1 DRY_RUN_OUT="$out" \
    bash "$root/scripts/deploy-from-release.sh" v0.20.1 > "$scratch/log" 2>&1 || rc=$?
  if [[ "$mode" == valid ]]; then
    [[ "$rc" == 0 ]] && cmp -s "$out" "$scratch/payload/bobbin" || {
      cat "$scratch/log"; echo 'FAIL: local verified control'; exit 1;
    }
  else
    [[ "$rc" != 0 && ! -e "$out" ]] || {
      cat "$scratch/log"; echo "FAIL: local $mode accepted"; exit 1;
    }
    grep -q 'REFUSED:' "$scratch/log"
  fi
  [[ ! -e "$scratch/downloaded" ]] || { echo 'FAIL: local handoff downloaded'; exit 1; }
  echo "PASS: local $mode, no download"
done

# Cutover hand-off: the real script, beside a stub cutover, must pass the cutover's
# exit status through AND remove its work dir. `exec` used to skip the EXIT trap, so
# every deploy leaked its work dir into /tmp (aegis-86f2v7.1).
mkdir -p "$scratch/handoff"
cp "$root/scripts/deploy-from-release.sh" "$scratch/handoff/"
printf '%s  %s\n' "$sum" "$asset" > "$scratch/release/SHA256SUMS.txt"
for want_rc in 0 3; do
  cat > "$scratch/handoff/deploy-cutover.sh" <<STUB
#!/usr/bin/env bash
[[ -x "\$1" ]] && "\$1" | grep -q 'bobbin 0.20.1' && echo seen > "$scratch/cutover-saw-binary"
exit $want_rc
STUB
  chmod +x "$scratch/handoff/deploy-cutover.sh"
  rm -rf "$scratch/tmp" "$scratch/cutover-saw-binary"; mkdir -p "$scratch/tmp"
  rc=0
  env PATH="$scratch/bin:/usr/bin:/bin" FIXTURE_RELEASE="$scratch/release" TMPDIR="$scratch/tmp" \
    DEPLOY_HOST=serving.test bash "$scratch/handoff/deploy-from-release.sh" v0.20.1 \
    > "$scratch/log" 2>&1 || rc=$?
  if [[ "$rc" != "$want_rc" || ! -e "$scratch/cutover-saw-binary" ]]; then
    cat "$scratch/log"; echo "FAIL: handoff rc=$rc want=$want_rc"; exit 1
  fi
  if [[ -n "$(ls -A "$scratch/tmp")" ]]; then
    ls -la "$scratch/tmp"; echo "FAIL: handoff rc=$want_rc left its work dir behind"; exit 1
  fi
  echo "PASS: handoff rc=$want_rc, work dir removed"
done
