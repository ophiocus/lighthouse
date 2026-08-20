#!/usr/bin/env bash
# gather.sh — one-shot fleet telemetry, emitted as a single JSON blob on stdout.
#
# Runs on the VPS *host* (over `ssh <host> bash -s`), so it sees what an
# unprivileged container cannot: docker state, host disk/mem/load, OS updates.
# It is read-only — inspects, never mutates. TLS domains are derived from the
# running containers' Traefik Host() labels, so a newly-deployed property is
# covered automatically with no list to maintain.
set -uo pipefail

json_str() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

echo "{"
printf '"generated":"%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# ── containers ──────────────────────────────────────────────────────────────
echo '"containers":['
first=1
for c in $(docker ps -a --format '{{.Names}}' | sort); do
  st=$(docker inspect -f '{{.State.Status}}' "$c" 2>/dev/null || echo missing)
  health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$c" 2>/dev/null || echo none)
  started=$(docker inspect -f '{{.State.StartedAt}}' "$c" 2>/dev/null || echo "")
  restarts=$(docker inspect -f '{{.RestartCount}}' "$c" 2>/dev/null || echo 0)
  image=$(docker inspect -f '{{.Config.Image}}' "$c" 2>/dev/null || echo "")
  [ $first -eq 1 ] && first=0 || echo ','
  printf '{"name":"%s","status":"%s","health":"%s","started":"%s","restarts":"%s","image":"%s"}' \
    "$(json_str "$c")" "$st" "$health" "$started" "$restarts" "$(json_str "$image")"
done
echo '],'

# ── drupal core / db / maintenance ──────────────────────────────────────────
echo '"drupal":['
first=1
for c in $(docker ps --format '{{.Names}}' | grep -- '-drupal-1' | sort); do
  ver=$(docker exec "$c" drush status --field=drupal-version 2>/dev/null | tr -d '[:space:]')
  maint=$(docker exec "$c" drush sget system.maintenance_mode 2>/dev/null | tr -d '[:space:]')
  db=$(docker exec "$c" drush status --field=db-status 2>/dev/null | tr -d '[:space:]')
  [ $first -eq 1 ] && first=0 || echo ','
  printf '{"name":"%s","core":"%s","maintenance":"%s","db":"%s"}' \
    "$(json_str "$c")" "${ver:-unknown}" "${maint:-0}" "${db:-unknown}"
done
echo '],'

# ── TLS expiry per public host (derived from Traefik labels) ────────────────
hosts=$(for c in $(docker ps --format '{{.Names}}'); do
  docker inspect "$c" --format '{{range .Config.Labels}}{{println .}}{{end}}' 2>/dev/null \
    | grep -oE 'Host\(`[^`]+`\)' | sed -E 's/Host\(`([^`]+)`\)/\1/'
done | grep -v '^www\.' | sort -u)
echo '"tls":['
first=1
for d in $hosts; do
  na=$(echo | timeout 8 openssl s_client -servername "$d" -connect "$d:443" 2>/dev/null \
        | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2)
  [ $first -eq 1 ] && first=0 || echo ','
  printf '{"domain":"%s","not_after":"%s"}' "$(json_str "$d")" "$(json_str "${na:-}")"
done
echo '],'

# ── host ────────────────────────────────────────────────────────────────────
disk=$(df -h / | awk 'NR==2{print $3"/"$2}')
disk_pct=$(df / | awk 'NR==2{gsub("%","",$5); print $5}')
mem_line=$(free -m | awk 'NR==2{print $3" "$2}')
mem_used=$(echo "$mem_line" | awk '{print $1}')
mem_total=$(echo "$mem_line" | awk '{print $2}')
mem_pct=$(( mem_total > 0 ? mem_used * 100 / mem_total : 0 ))
load1=$(cut -d' ' -f1 /proc/loadavg)
uptime_s=$(uptime -p 2>/dev/null | sed 's/^up //' || echo "-")
upg=$(apt list --upgradable 2>/dev/null | grep -c upgradable)
sec=$(apt list --upgradable 2>/dev/null | grep -ci security)
rb=$([ -f /var/run/reboot-required ] && echo YES || echo no)
printf '"host":{"disk":"%s","disk_pct":%s,"mem":"%s / %s MB","mem_pct":%s,"load":"%s","uptime":"%s","upgradable":%s,"security":%s,"reboot":"%s"}\n' \
  "$disk" "${disk_pct:-0}" "$mem_used" "$mem_total" "${mem_pct:-0}" "$load1" "$(json_str "$uptime_s")" "${upg:-0}" "${sec:-0}" "$rb"
echo "}"
