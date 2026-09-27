#!/usr/bin/env sh
# Monthly restore test (run from deploy/, e.g. by cron on the 1st; ADR 0015).
#
# Restores the newest backup into a scratch database next to the live one,
# checks that the history tables came back and are not ahead of the live
# database, checks that the backed-up master key is the one in use, then
# drops the scratch database. Exit status 0 means the backup restores.
set -eu
cd "$(dirname "$0")"
[ -f .env ] && set -a && . ./.env && set +a
dir="${1:-backups}"
scratch="qd_restore_test"
dump="$(ls -t "$dir"/quantdesk-*.dump* 2>/dev/null | head -n 1)"
[ -n "$dump" ] || { echo "no backup in $dir" >&2; exit 1; }
work="$(mktemp -d)"
trap 'rm -rf "$work"; docker compose exec -T postgres dropdb -U quantdesk --if-exists "$scratch" >/dev/null 2>&1 || true' EXIT

decrypt() {
    case "$1" in
        *.enc) openssl enc -d -aes-256-cbc -pbkdf2 -iter 600000 -pass env:QD_BACKUP_PASSPHRASE -in "$1" -out "$2" ;;
        *) cp "$1" "$2" ;;
    esac
}

decrypt "$dump" "$work/db.dump"
docker compose exec -T postgres dropdb -U quantdesk --if-exists "$scratch"
docker compose exec -T postgres createdb -U quantdesk "$scratch"
docker compose exec -T postgres pg_restore -U quantdesk --exit-on-error -d "$scratch" < "$work/db.dump"

count() {
    docker compose exec -T postgres psql -U quantdesk -d "$1" -Atc "SELECT count(*) FROM $2"
}
fail=0
for table in journal audit_log settings_versions halt_events; do
    restored="$(count "$scratch" "$table")"
    live="$(count quantdesk "$table")"
    echo "$table: restored $restored, live $live"
    # History tables only grow: a restored copy can be behind, never ahead.
    [ "$restored" -le "$live" ] || fail=1
done
[ "$(count "$scratch" journal)" -gt 0 ] || { echo "the restored journal is empty" >&2; fail=1; }

key="$(ls -t "$dir"/master-key-* 2>/dev/null | head -n 1)"
if [ -n "$key" ]; then
    decrypt "$key" "$work/master.key"
    docker compose cp qd-server:/var/lib/quantdesk/master.key "$work/live.key"
    if cmp -s "$work/master.key" "$work/live.key"; then
        echo "master key backup: matches the key in use"
    else
        echo "master key backup: DIFFERS from the key in use (rotated since?); run backup.sh now" >&2
        fail=1
    fi
fi

if [ "$fail" -eq 0 ]; then echo "restore test passed: $dump"; else echo "restore test FAILED: $dump" >&2; fi
exit "$fail"
