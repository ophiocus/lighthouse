# Lighthouse

Fleet health telemetry for the Tecnocrática VPS — a native desktop board that
shows, at a glance, whether every property on the node is up, current, and
answering.

Bootstrapped from `rust-skeleton` (eframe/egui, WiX MSI, GitHub self-update).

## Why a desktop app (and not a hosted dashboard)

The tool runs **off the server**, on the operator's workstation, and reaches the
VPS over SSH. That single choice dissolves the two problems a server-hosted
board would have:

- **No recursion.** A dashboard hosted *on* the node can't report the node's own
  outage. Lighthouse runs elsewhere, so a dead node reads as "unreachable"
  rather than going dark with everything else.
- **No container boundary.** An unprivileged container can't see host disk, OS
  updates, or `docker` state. `ssh <host> 'docker inspect / df / apt …'` lands
  on the **host**, so the full picture is available with **no new VPS surface**:
  no exposed endpoint, no `docker.sock` mount, no per-property agent. It reuses
  the SSH key already in `~/.ssh/config`.

## What it collects

Two sources, merged by property:

| Source | Runs where | Provides |
| --- | --- | --- |
| HTTPS probe (`reqwest`) | workstation | status code, latency, true external reachability |
| `scripts/gather.sh` over SSH | VPS host | container state/health/restarts, Drupal core/db/maintenance, TLS expiry, host disk/mem/load/OS-updates/reboot |

The gather script is **read-only** and derives its TLS target list from the
running containers' Traefik `Host()` labels, so a newly-deployed property is
covered automatically — nothing to hand-maintain.

Per-stack liveness is registry-driven: Drupal answers at `/`, the myevery API
service answers at `/healthz`. See `src/registry.rs`.

## Run

```
cargo run                 # the GUI board
cargo run -- --probe      # headless one-shot: gather, print, exit (cron/CI)
```

Debug builds keep a console (panics, `--probe` output are visible); release
builds are windowed (`windows_subsystem = "windows"`).

## Config

`%APPDATA%\Lighthouse\config.json`:

| Key | Default | Meaning |
| --- | --- | --- |
| `host_alias` | `tecnocratica_node_1` | SSH host alias to gather from |
| `auto_refresh_secs` | `0` | auto-refresh cadence (0 = manual only) |
| `dark_mode` | `true` | theme |
| `zoom` | `1.0` | UI zoom factor |

## Layout

- `src/telemetry.rs` — the collector (SSH + HTTP + merge, health & gap derivation)
- `src/model.rs` — raw gather shape + derived render model
- `src/registry.rs` — the property registry
- `src/dates.rs` — dependency-free `notAfter` → days-remaining
- `src/app.rs` — the egui board
- `scripts/gather.sh` — the host-side read-only gather (embedded via `include_str!`)
