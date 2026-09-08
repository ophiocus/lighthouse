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
#
# Per-project collection runs in PARALLEL. Serially it took ~37s on a six-project
# node, which reads as a hung board: nearly all of it is `docker exec … drush`,
# each of which boots PHP inside a container. The work is per-project and shares
# no state, so each project writes its own JSON fragment to a temp file and the
# fragments are concatenated in order afterwards — output stays deterministic
# while wall-clock collapses to roughly the slowest single project.
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

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Everything expensive for one project. Writes one JSON object to $TMP/$idx.
project_json() {
  local d="$1" idx="$2"
  local f="${d}docker-compose.yaml"
  local slug
  slug=$(sed -nE 's/^name:[[:space:]]*tec-([a-z0-9-]+).*/\1/p' "$f" | head -1)
  [ -n "$slug" ] || return 0
  # The site directory is named for the property's real apex. Used below to pick
  # the probe target, and reported as the "apex" field.
  local apex; apex=$(basename "$d")

  local services type svc probe
  services=$(cd "$d" && docker compose config --services 2>/dev/null)
  if grep -qx drupal <<<"$services"; then
    type=drupal; svc=drupal; probe="/"
  else
    type=node; svc=$(grep -vx db <<<"$services" | head -1); probe="/healthz"
  fi
  [ -n "$svc" ] || svc=app
  local container="tec-${slug}-${svc}-1"
  local dbc=""; grep -qx db <<<"$services" && dbc="tec-${slug}-db-1"

  # One docker inspect for all container state, rather than five.
  local insp st health started restarts image
  insp=$(docker inspect -f '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.State.StartedAt}}|{{.RestartCount}}|{{.Config.Image}}' "$container" 2>/dev/null)
  IFS='|' read -r st health started restarts image <<<"${insp:-missing|none|||}"
  [ -n "$st" ] || st=missing
  [ -n "$health" ] || health=none
  [ -n "$restarts" ] || restarts=0

  # Public hosts from Traefik labels. The probe target is the property's own
  # apex whenever Traefik serves it — picking the alphabetically first label
  # instead sends the probe to a sibling domain, and for a property whose alias
  # still points at registrar parking that answers 200 from a page which is not
  # the site (false green, and no TLS date at all). Fall back to the first
  # non-www/non-api label only when the apex is NOT served here: a property may
  # legitimately be reached at a host other than its directory name.
  local hosts primary url h
  mapfile -t hosts < <(docker inspect "$container" --format '{{range .Config.Labels}}{{println .}}{{end}}' 2>/dev/null \
      | grep -oE 'Host\(`[^`]+`\)' | sed -E 's/.*`([^`]+)`.*/\1/' | sort -u)
  primary=""
  printf '%s\n' "${hosts[@]}" | grep -qxF "$apex" && primary="$apex"
  if [ -z "$primary" ]; then
    for h in "${hosts[@]}"; do case "$h" in www.*|api.*) ;; *) primary="$h"; break;; esac; done
  fi
  [ -z "$primary" ] && primary="${hosts[0]:-}"
  url=""; [ -n "$primary" ] && url="https://$primary"

  local core="null" maint="null" dbstatus="null"
  if [ "$type" = drupal ]; then
    # Two drush calls, not three: `status` yields version and db-status
    # together. Each call boots PHP in the container, so the saving is real.
    local stat v s m
    # `--format=csv` emits a header row before the values; take the last line.
    stat=$(docker exec "$container" drush status --fields=drupal-version,db-status --format=csv 2>/dev/null | tr -d '\r' | tail -1)
    v=$(printf '%s' "$stat" | cut -d, -f1 | tr -d '[:space:]')
    s=$(printf '%s' "$stat" | cut -d, -f2 | tr -d '[:space:]')
    m=$(docker exec "$container" drush sget system.maintenance_mode 2>/dev/null | tr -d '[:space:]')
    core="\"$(json_str "${v:-unknown}")\""
    maint="\"$(json_str "${m:-0}")\""
    dbstatus="\"$(json_str "${s:-unknown}")\""
  fi

  local tls=""
  if [ -n "$primary" ]; then
    tls=$(echo | timeout 8 openssl s_client -servername "$primary" -connect "$primary:443" 2>/dev/null \
          | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2)
  fi

  printf '{"slug":"%s","type":"%s","service":"%s","container":"%s","db_container":"%s","apex":"%s","url":"%s","probe_path":"%s","domains":%s,"status":"%s","health":"%s","started":"%s","restarts":"%s","image":"%s","core":%s,"maintenance":%s,"db_status":%s,"tls":"%s"}' \
    "$(json_str "$slug")" "$type" "$(json_str "$svc")" "$(json_str "$container")" "$(json_str "$dbc")" \
    "$(json_str "$apex")" "$(json_str "$url")" "$probe" "$(json_arr "${hosts[@]}")" \
    "$st" "$health" "$started" "$restarts" "$(json_str "$image")" \
    "$core" "$maint" "$dbstatus" "$(json_str "$tls")" > "$TMP/$idx"
}

idx=0
for d in "$SITES"/*/; do
  [ -f "${d}docker-compose.yaml" ] || continue
  idx=$((idx + 1))
  project_json "$d" "$(printf '%03d' "$idx")" &
done
wait

# Concatenate in directory order so output is stable run to run.
first=1
for f in "$TMP"/*; do
  [ -s "$f" ] || continue
  [ $first -eq 1 ] && first=0 || echo ','
  cat "$f"
done
echo
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
