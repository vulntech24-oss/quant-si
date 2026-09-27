#!/usr/bin/env sh
# Daily logical backup of the QuantDesk database (run from deploy/, e.g. by cron).
# The journal and every history table are append-only, so a dump is a
# complete record of decisions, orders, halts, evidence and audits.
set -eu
out="${1:-backups}"
mkdir -p "$out"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
docker compose exec -T postgres pg_dump -U quantdesk --format=custom quantdesk > "$out/quantdesk-$stamp.dump"
# Keep 30 days.
find "$out" -name 'quantdesk-*.dump' -mtime +30 -delete
echo "$out/quantdesk-$stamp.dump"
