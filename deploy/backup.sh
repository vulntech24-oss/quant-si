#!/usr/bin/env sh
# Daily backup of QuantDesk (run from deploy/, e.g. by cron; ADR 0015).
#
# 1. pg_dump of the database (custom format). The journal and every history
#    table are append-only, so a dump is the complete record.
# 2. A copy of the secrets master key, which never goes into the database.
# 3. With QD_BACKUP_PASSPHRASE set, both are encrypted (AES-256, PBKDF2)
#    before they leave the host.
# 4. With QD_BACKUP_REMOTE set (an rclone remote such as "b2:qd-backups",
#    configured once with `rclone config`), both are copied off-site: the
#    dump under db/, the key under keys/ (or QD_BACKUP_KEY_REMOTE, ideally a
#    different provider, so one leak never holds both).
#
# Local copies are kept 30 days; off-site copies QD_BACKUP_KEEP_DAYS (90).
set -eu
cd "$(dirname "$0")"
[ -f .env ] && set -a && . ./.env && set +a
out="${1:-backups}"
mkdir -p "$out"
chmod 700 "$out"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
dump="$out/quantdesk-$stamp.dump"
key="$out/master-key-$stamp.hex"

docker compose exec -T postgres pg_dump -U quantdesk --format=custom quantdesk > "$dump"
if docker compose cp qd-server:/var/lib/quantdesk/master.key "$key" 2>/dev/null; then
    chmod 600 "$key"
else
    echo "warning: no master key file (QD_MASTER_KEY in use?): back it up yourself" >&2
    key=""
fi

if [ -n "${QD_BACKUP_PASSPHRASE:-}" ]; then
    for f in "$dump" $key; do
        openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt -pass env:QD_BACKUP_PASSPHRASE -in "$f" -out "$f.enc"
        rm -f "$f"
    done
    dump="$dump.enc"
    [ -n "$key" ] && key="$key.enc"
elif [ -n "${QD_BACKUP_REMOTE:-}" ]; then
    echo "refusing to copy unencrypted backups off-site: set QD_BACKUP_PASSPHRASE" >&2
    exit 1
fi

if [ -n "${QD_BACKUP_REMOTE:-}" ]; then
    keep="${QD_BACKUP_KEEP_DAYS:-90}"
    key_remote="${QD_BACKUP_KEY_REMOTE:-$QD_BACKUP_REMOTE/keys}"
    rclone copy "$dump" "$QD_BACKUP_REMOTE/db/"
    [ -n "$key" ] && rclone copy "$key" "$key_remote/"
    rclone delete --min-age "${keep}d" "$QD_BACKUP_REMOTE/db/"
    rclone delete --min-age "${keep}d" "$key_remote/"
fi

find "$out" -name 'quantdesk-*' -mtime +30 -delete
find "$out" -name 'master-key-*' -mtime +30 -delete
echo "$dump"
