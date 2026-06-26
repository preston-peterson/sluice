#!/usr/bin/env bash
# Fetch the DB-IP Lite IP-to-Country database for Sluice's offline geo lookup (FR-052).
#
# The data is per-machine and NOT committed to the repo. This installs the current (or previous)
# month's free CSV where Sluice reads it: $XDG_DATA_HOME/sluice/geoip/dbip-country-lite.csv. At
# runtime Sluice only reads this local file — no network calls (SEC-007). If it's absent, country
# lookups are simply disabled (shown as "—").
#
# Source:  https://db-ip.com/db/download/ip-to-country-lite
# License: CC-BY-4.0 — attribution required: "IP geolocation by DB-IP" (https://db-ip.com)
set -euo pipefail

dest_dir="${XDG_DATA_HOME:-$HOME/.local/share}/sluice/geoip"
dest="$dest_dir/dbip-country-lite.csv"
mkdir -p "$dest_dir"

ym_now="$(date -u +%Y-%m)"
# previous month (GNU date, then BSD/macOS date as a fallback)
ym_prev="$(date -u -d 'last month' +%Y-%m 2>/dev/null || date -u -v-1m +%Y-%m 2>/dev/null || echo "")"

for ym in "$ym_now" "$ym_prev"; do
  [ -n "$ym" ] || continue
  url="https://download.db-ip.com/free/dbip-country-lite-$ym.csv.gz"
  echo "Trying $url"
  if curl -fSL "$url" -o "$dest_dir/dbip.csv.gz"; then
    gunzip -f "$dest_dir/dbip.csv.gz"
    mv -f "$dest_dir/dbip.csv" "$dest"
    echo "Installed: $dest"
    echo 'Attribution: "IP geolocation by DB-IP" (https://db-ip.com), CC-BY-4.0.'
    exit 0
  fi
done

echo "Could not download the DB-IP Lite country database." >&2
echo "Get it manually from https://db-ip.com/db/download/ip-to-country-lite and save the CSV to:" >&2
echo "  $dest" >&2
exit 1
