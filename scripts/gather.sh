#!/usr/bin/env bash
# gather.sh — discover every project on the VPS and report its telemetry as
# one JSON blob on stdout.
#
# Discovery, not a hardcoded list: it walks the site directories, and the
# compose file classifies each project — a stack with a `drupal` service is a
# Drupal system, a single-app stack is a Node system. Container name, public
# URL, and probe path are all derived from that classification, so a
# newly-deployed project of either type is picked up automatically.
#
# Read-only. Inspects; never mutates.
set -uo pipefail

SITES="${TEC_SITES_ROOT:-/srv/tecnocratica/sites}"

json_str() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
json_arr() { # each arg → quoted element
  local out="" x
  for x in "$@"; do out="$out,\"$(json_str "$x")\""; done
  printf '[%s]' "${out:1}"
}

echo "{"
printf '"generated":"%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo '"projects":['

first=1
for d in "$SITES"/*/; do
  f="${d}docker-compose.yaml"
  [ -f "$f" ] || continue
  slug=$(sed -nE 's/^name:[[:space:]]*tec-([a-z0-9-]+).*/\1/p' "$f" | head -1)
  [ -n "$slug" ] || continue

  services=$(cd "$d" && docker compose config --services 2>/dev/null)
  if grep -qx drupal <<<"$services"; then
    type=drupal; svc=drupal; probe="/"
  else
    type=node; svc=$(grep -vx db <<<"$services" | head -1); probe="/healthz"
  fi
  [ -n "$svc" ] || svc=app
  container="tec-${slug}-${svc}-1"
  dbc=""; grep -qx db <<<"$services" && dbc="tec-${slug}-db-1"

  st=$(docker inspect -f '{{.State.Status}}' "$container" 2>/dev/null || echo missing)
  health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$container" 2>/dev/null || echo none)
  started=$(docker inspect -f '{{.State.StartedAt}}' "$container" 2>/dev/null || echo "")
  restarts=$(docker inspect -f '{{.RestartCount}}' "$container" 2>/dev/null || echo 0)
  image=$(docker inspect -f '{{.Config.Image}}' "$container" 2>/dev/null || echo "")

  # Public hosts from Traefik labels; primary = first non-www, non-api.
  mapfile -t hosts < <(docker inspect "$container" --format '{{range .Config.Labels}}{{println .}}{{end}}' 2>/dev/null \
      | grep -oE 'Host\(`[^`]+`\)' | sed -E 's/.*`([^`]+)`.*/\1/' | sort -u)
  primary=""
  for h in "${hosts[@]}"; do case "$h" in www.*|api.*) ;; *) primary="$h"; break;; esac; done
  [ -z "$primary" ] && primary="${hosts[0]:-}"
  url=""; [ -n "$primary" ] && url="https://$primary"

  core="null"; maint="null"; dbstatus="null"
  if [ "$type" = drupal ]; then
    v=$(docker exec "$container" drush status --field=drupal-version 2>/dev/null | tr -d '[:space:]')
    m=$(docker exec "$container" drush sget system.maintenance_mode 2>/dev/null | tr -d '[:space:]')
    s=$(docker exec "$container" drush status --field=db-status 2>/dev/null | tr -d '[:space:]')
    core="\"$(json_str "${v:-unknown}")\""
    maint="\"$(json_str "${m:-0}")\""
    dbstatus="\"$(json_str "${s:-unknown}")\""
  fi

  tls=""
  if [ -n "$primary" ]; then
    tls=$(echo | timeout 8 openssl s_client -servername "$primary" -connect "$primary:443" 2>/dev/null \
          | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2)
  fi

  [ $first -eq 1 ] && first=0 || echo ','
  printf '{"slug":"%s","type":"%s","service":"%s","container":"%s","db_container":"%s","apex":"%s","url":"%s","probe_path":"%s","domains":%s,"status":"%s","health":"%s","started":"%s","restarts":"%s","image":"%s","core":%s,"maintenance":%s,"db_status":%s,"tls":"%s"}' \
    "$(json_str "$slug")" "$type" "$(json_str "$svc")" "$(json_str "$container")" "$(json_str "$dbc")" \
    "$(json_str "$(basename "$d")")" "$(json_str "$url")" "$probe" "$(json_arr "${hosts[@]}")" \
    "$st" "$health" "$started" "$restarts" "$(json_str "$image")" \
    "$core" "$maint" "$dbstatus" "$(json_str "$tls")"
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
