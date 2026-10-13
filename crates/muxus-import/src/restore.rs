//! The conflict-safe restore of a portable connection set
//! (`restoreImportedConnections` / `restoreConnections` in
//! `client/src/data-transfer.ts`), as a plan of store writes.
//!
//! The TypeScript issues the writes as HTTP requests; here the plan lists
//! them in the same order for the backend to apply. ssh_config edits stay
//! sequential: every write atomically rewrites a config file.

use std::collections::{HashMap, HashSet};

use muxus_core::api::{
    HostOrderRequest, HostUpsertRequest, OpenSshMetadataPatch, SavedHostProfile,
    SavedHostProfileInput, SshHostEntry,
};
use serde::{Deserialize, Serialize};

use crate::portable::PortableConnections;

/// What happens to an imported host that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictStrategy {
    #[default]
    Keep,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RestoreResult {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
}

impl RestoreResult {
    /// `2 added · 1 replaced · 3 kept`, leaving out zero counts.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.added > 0 {
            parts.push(format!("{} added", self.added));
        }
        if self.updated > 0 {
            parts.push(format!("{} replaced", self.updated));
        }
        if self.skipped > 0 {
            parts.push(format!("{} kept", self.skipped));
        }
        parts.join(" · ")
    }
}

/// The hosts Muxus already has, keyed the way imports are matched: ssh_config
/// hosts by every alias, saved hosts by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExistingHosts {
    /// Alias → (the block's primary alias, its config file).
    ssh_by_alias: HashMap<String, (String, String)>,
    profile_ids: HashSet<String>,
}

impl ExistingHosts {
    pub fn new(ssh_hosts: &[SshHostEntry], saved_profiles: &[SavedHostProfile]) -> Self {
        let mut ssh_by_alias = HashMap::new();
        for host in ssh_hosts {
            for alias in &host.aliases {
                ssh_by_alias.insert(alias.clone(), (host.alias.clone(), host.file.clone()));
            }
        }
        Self {
            ssh_by_alias,
            profile_ids: saved_profiles
                .iter()
                .map(|profile| profile.id.clone())
                .collect(),
        }
    }

    pub fn has_alias(&self, alias: &str) -> bool {
        self.ssh_by_alias.contains_key(alias)
    }

    pub fn has_profile(&self, id: &str) -> bool {
        self.profile_ids.contains(id)
    }
}

/// One write, in the order the restore performs them.
#[derive(Debug, Clone, PartialEq)]
pub enum RestoreStep {
    /// Create or replace a Host block (`POST /api/ssh/config/hosts`), then,
    /// when `metadata` is set, patch the Muxus metadata of `alias`.
    UpsertSshHost {
        request: HostUpsertRequest,
        alias: String,
        metadata: Option<OpenSshMetadataPatch>,
    },
    /// Create or replace a saved host under its ID (`PUT /api/profiles`),
    /// then patch its metadata.
    PutSavedHost {
        input: SavedHostProfileInput,
        metadata: OpenSshMetadataPatch,
    },
    /// Replace the sidebar order (`PUT /api/hosts/order`).
    SetHostOrder(HostOrderRequest),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RestorePlan {
    pub steps: Vec<RestoreStep>,
    /// The counts the user is told once every step succeeded.
    pub result: RestoreResult,
}

/// Plan restoring `data`: existing hosts are kept or replaced per `conflicts`.
pub fn plan_restore(
    data: &PortableConnections,
    existing: &ExistingHosts,
    conflicts: ConflictStrategy,
) -> RestorePlan {
    let mut plan = RestorePlan::default();

    for host in &data.ssh_hosts {
        let current = existing.ssh_by_alias.get(&host.alias);
        if current.is_some() && conflicts == ConflictStrategy::Keep {
            plan.result.skipped += 1;
            continue;
        }
        let (previous_alias, file) = match current {
            Some((alias, file)) => (Some(alias.clone()), Some(file.clone())),
            None => (None, None),
        };
        plan.steps.push(RestoreStep::UpsertSshHost {
            request: HostUpsertRequest {
                aliases: host.aliases.clone(),
                description: host.description.clone(),
                file,
                options: host.options.clone(),
                previous_alias,
            },
            alias: host.alias.clone(),
            metadata: host.metadata.as_ref().map(|metadata| metadata.to_patch()),
        });
        if current.is_some() {
            plan.result.updated += 1;
        } else {
            plan.result.added += 1;
        }
    }

    for profile in &data.saved_hosts {
        let exists = existing.has_profile(&profile.id);
        if exists && conflicts == ConflictStrategy::Keep {
            plan.result.skipped += 1;
            continue;
        }
        plan.steps.push(RestoreStep::PutSavedHost {
            input: SavedHostProfileInput {
                id: Some(profile.id.clone()),
                name: profile.name.clone(),
                profile: profile.profile.clone(),
            },
            metadata: profile.metadata.to_patch(),
        });
        if exists {
            plan.result.updated += 1;
        } else {
            plan.result.added += 1;
        }
    }

    if conflicts == ConflictStrategy::Replace && !data.host_order.is_empty() {
        plan.steps.push(RestoreStep::SetHostOrder(HostOrderRequest {
            hosts: data.host_order.clone(),
        }));
    }
    plan
}
