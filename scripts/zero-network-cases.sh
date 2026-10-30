# SPDX-License-Identifier: Apache-2.0
# Per-subcommand zero-network cases, shared by zero-network.sh on Linux
# (strace) and macOS (sandbox-exec). Sourced, not run.
#
# Each case is one line `zn_case <name> <command...>`. The caller defines
# zn_case: it runs the command under its observer, checks the observer saw no
# AF_INET/AF_INET6 socket, connect, send, bind or DNS event, and leaves the
# stdout in "$work/<name>.stdout" for the next case to read (tokens, ids).
# Case names are what scripts/check-network-coverage.py matches against
# `albumen --help` and the albumen-mcp tools/list: `<subcommand>` or
# `<subcommand>-<action>`, and `mcp-<tool>` for MCP tools.
#
# Everything is synthetic and lives under "$work". Network-allowed modes
# (export to s3://, family publish, mcp s3) are not here; they belong to
# no-unexpected-egress.sh with an allow-list.

zn_fixtures() {
  mkdir -p "$work/takeout/Takeout/Google Photos/Album" "$work/icloud" "$work/keys" "$work/receiver"
  printf 'albumen synthetic takeout original. Not a photograph.\n' \
    >"$work/takeout/Takeout/Google Photos/Album/IMG_0002.jpg"
  printf '{"title":"IMG_0002.jpg","photoTakenTime":{"timestamp":"1700000000"}}\n' \
    >"$work/takeout/Takeout/Google Photos/Album/IMG_0002.jpg.json"
  printf 'albumen synthetic iCloud original. Not a photograph.\n' >"$work/icloud/IMG_0003.jpg"
  printf 'albumen synthetic received original. Not a photograph.\n' >"$work/receiver/IMG_0004.jpg"
  printf 'albumen synthetic family original. Not a photograph.\n' >"$work/family-photo.jpg"
  printf 'albumen synthetic family original 2. Not a photograph.\n' >"$work/family-photo-2.jpg"
}

# Last whitespace-separated field of the first stdout line starting with $2.
zn_field() {
  awk -v want="$2" '$1 == want { print $NF; exit }' "$work/$1.stdout"
}

zn_cases() {
  local albumen="$1"
  zn_case export "$albumen" export --from "$work/library" --to "$work/out/export" --apply
  zn_case verify-manifest "$albumen" verify-manifest \
    --manifest "$work/out/export/ALBUMEN-MANIFEST.txt" --against "$work/out/export"
  zn_case import-takeout "$albumen" import-takeout --from "$work/takeout" \
    --to "$work/out/takeout-library" --report "$work/out/takeout.jsonl" --apply
  zn_case import-icloud "$albumen" import-icloud --from "$work/icloud" \
    --to "$work/out/icloud-library" --report "$work/out/icloud.jsonl" --apply
  zn_case ledger "$albumen" ledger --library "$work/library"
  zn_case platform-dirs "$albumen" platform-dirs
  zn_case completions "$albumen" completions bash
  zn_case receive-hide "$albumen" receive hide --library "$work/receiver" --name IMG_0004.jpg --apply
  zn_case receive-unhide "$albumen" receive unhide --library "$work/receiver" --name IMG_0004.jpg --apply

  zn_case family-keygen "$albumen" family keygen --key "$work/keys/a.key" --apply
  zn_case family-keygen-b "$albumen" family keygen --key "$work/keys/b.key" --apply
  local member_b
  member_b="$(zn_field family-keygen-b member)"
  zn_case family-init "$albumen" family init --to "$work/out/family" --key "$work/keys/a.key" --apply
  local library
  library="$(zn_field family-init library)"
  zn_case family-add "$albumen" family add --library "$library" --key "$work/keys/a.key" \
    --from "$work/family-photo.jpg" --apply
  local sha
  sha="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/family-photo.jpg")"
  zn_case family-invite "$albumen" family invite --library "$library" --key "$work/keys/a.key" \
    --member "$member_b" --apply
  local token
  token="$(zn_field family-invite token)"
  zn_case family-join "$albumen" family join --library "$library" --key "$work/keys/b.key" \
    --token "$token" --name synthetic-b --apply
  zn_case family-admit "$albumen" family admit --library "$library" --key "$work/keys/a.key" \
    --member "$member_b" --token "$token" --apply
  zn_case family-album-add "$albumen" family album-add --library "$library" --key "$work/keys/a.key" \
    --album synthetic --sha256 "$sha" --apply
  local add_id
  add_id="$(zn_field family-album-add add)"
  zn_case family-album-omit "$albumen" family album-omit --library "$library" --key "$work/keys/a.key" \
    --add "$add_id" --apply
  zn_case family-hide "$albumen" family hide --library "$library" --key "$work/keys/a.key" --sha256 "$sha" --apply
  zn_case family-manifest "$albumen" family manifest --library "$library" --key "$work/keys/a.key" --apply
  zn_case family-export "$albumen" family export --library "$library" --to "$work/out/family-export" --apply
  # A second live copy, one add behind, so sync has something to add.
  local copy="$work/out/family-copy/$(basename "$library")"
  mkdir -p "$work/out/family-copy"
  cp -R "$library" "$copy"
  zn_case family-add-2 "$albumen" family add --library "$library" --key "$work/keys/a.key" \
    --from "$work/family-photo-2.jpg" --apply
  zn_case family-sync "$albumen" family sync --from "$library" --to "$copy" --apply
  zn_case family-policy "$albumen" family policy --bucket synthetic-bucket --member "$member_b"
  zn_case family-tombstone "$albumen" family tombstone --library "$library" --key "$work/keys/a.key" \
    --sha256 "$sha" --apply
  zn_case family-revoke "$albumen" family revoke --library "$library" --key "$work/keys/a.key" \
    --member "$member_b" --apply
}
