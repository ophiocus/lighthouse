//! Control templates — the per-type definition of what a project's control
//! interface offers. A [`ProjectType`] maps to an ordered list of [`Action`]s;
//! each action knows its label, whether it mutates state, its confirmation
//! copy, and the exact remote command to run over SSH.
//!
//! Safety: every mutating action is `danger()` and gated behind a confirm step
//! in the UI. `MaintenanceOff` is included as an explicit operator action — the
//! app never fires it on its own; lifting maintenance is always a human click.

use crate::model::{Project, ProjectType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // Drupal
    DrushCacheRebuild,
    DrushDeploy,
    MaintenanceOn,
    MaintenanceOff,
    // both / Node
    Redeploy,
    RestartContainer,
    ViewLogs,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::DrushCacheRebuild => "Rebuild cache",
            Action::DrushDeploy => "Drush deploy",
            Action::MaintenanceOn => "Maintenance ON",
            Action::MaintenanceOff => "Maintenance OFF",
            Action::Redeploy => "Pull & redeploy",
            Action::RestartContainer => "Restart",
            Action::ViewLogs => "Logs",
        }
    }

    /// A mutating action — must be confirmed and is styled as dangerous.
    pub fn danger(self) -> bool {
        !matches!(self, Action::ViewLogs | Action::DrushCacheRebuild)
    }

    /// One-line confirmation shown in the modal.
    pub fn confirm(self, p: &Project) -> String {
        match self {
            Action::DrushCacheRebuild => format!("Rebuild the Drupal cache on {}?", p.slug),
            Action::DrushDeploy => {
                format!("Run `drush deploy` (updb + cim + cr) on {}?", p.slug)
            }
            Action::MaintenanceOn => {
                format!("Put {} into maintenance mode? Visitors will see the offline page.", p.slug)
            }
            Action::MaintenanceOff => format!(
                "Lift maintenance mode on {} and return it to visitors?",
                p.slug
            ),
            Action::Redeploy => format!(
                "Pull the latest image and recreate {}'s container?{}",
                p.slug,
                if p.kind == "drupal" { " Runs drush deploy after." } else { "" }
            ),
            Action::RestartContainer => format!("Restart the {} container?", p.container),
            Action::ViewLogs => format!("Show the last 200 log lines for {}?", p.container),
        }
    }

    /// The remote command to execute (over `ssh <host> bash -lc '<cmd>'`).
    pub fn command(self, p: &Project) -> String {
        let c = &p.container;
        let apex = format!("/srv/tecnocratica/sites/{}", p.apex);
        match self {
            Action::DrushCacheRebuild => format!("docker exec {c} drush cr"),
            Action::DrushDeploy => format!("docker exec {c} drush deploy"),
            Action::MaintenanceOn => {
                format!("docker exec {c} drush sset system.maintenance_mode 1 && docker exec {c} drush cr")
            }
            Action::MaintenanceOff => {
                format!("docker exec {c} drush sset system.maintenance_mode 0 && docker exec {c} drush cr")
            }
            Action::Redeploy => {
                let base = format!("cd {apex} && docker compose pull && docker compose up -d");
                if p.kind == "drupal" {
                    format!("{base} && docker exec {c} drush deploy")
                } else {
                    base
                }
            }
            Action::RestartContainer => format!("docker restart {c}"),
            Action::ViewLogs => format!("docker logs --tail 200 {c} 2>&1"),
        }
    }
}

/// The control template: which actions a project of this type exposes, in order.
pub fn actions_for(t: ProjectType) -> &'static [Action] {
    match t {
        ProjectType::Drupal => &[
            Action::DrushCacheRebuild,
            Action::DrushDeploy,
            Action::MaintenanceOn,
            Action::MaintenanceOff,
            Action::Redeploy,
            Action::ViewLogs,
        ],
        ProjectType::Node => &[
            Action::RestartContainer,
            Action::Redeploy,
            Action::ViewLogs,
        ],
        ProjectType::Unknown => &[Action::ViewLogs],
    }
}
