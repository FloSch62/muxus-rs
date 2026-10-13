//! Applying a reviewed import through the backend (`restoreImportedConnections`
//! in `client/src/data-transfer.ts`): the hosts Muxus has right now decide
//! what is kept or replaced, then the planned writes run in order. ssh_config
//! edits stay sequential: every write atomically rewrites a config file.

use muxus_import::review::ImportReview;
use muxus_import::{ExistingHosts, RestorePlan, RestoreResult, RestoreStep};
use muxus_session::{ApiResult, Backend};

/// The hosts an import is matched against: every ssh_config alias and every
/// saved host ID, read now.
pub async fn existing_hosts(backend: &Backend) -> ApiResult<ExistingHosts> {
    let config = backend.ssh_config().await?;
    let saved = backend.saved_hosts().await?;
    Ok(ExistingHosts::new(&config.hosts, &saved.profiles))
}

/// Run a plan's writes in order and return its counts.
pub async fn apply_restore_plan(backend: &Backend, plan: &RestorePlan) -> ApiResult<RestoreResult> {
    for step in &plan.steps {
        match step {
            RestoreStep::UpsertSshHost {
                request,
                alias,
                metadata,
            } => {
                backend.upsert_ssh_host(request.clone()).await?;
                if let Some(patch) = metadata {
                    backend
                        .patch_ssh_host_metadata(alias, patch.clone())
                        .await?;
                }
            }
            RestoreStep::PutSavedHost { input, metadata } => {
                let saved = backend.save_saved_host(input.clone()).await?;
                backend
                    .patch_saved_host_metadata(&saved.id, metadata.clone())
                    .await?;
            }
            RestoreStep::SetHostOrder(order) => backend.put_host_order(order.clone()).await?,
        }
    }
    Ok(plan.result)
}

/// Import the selected sessions of a review: plan against the hosts Muxus
/// has now, then write. `None` when nothing is selected.
pub async fn import_reviewed(
    backend: &Backend,
    review: &ImportReview,
) -> ApiResult<Option<RestoreResult>> {
    let existing = existing_hosts(backend).await?;
    match review.plan(&existing) {
        Some(plan) => apply_restore_plan(backend, &plan).await.map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use muxus_import::review::ImportVendor;
    use muxus_import::{ConflictStrategy, ImportedSshStorage};
    use muxus_session::{BackendOptions, RuntimeChoice};

    use super::*;

    const MOBAXTERM: &str = "[Bookmarks]
SubRep=Production\\Europe
Prod SSH=#109#0%prod.example.com%2222%deploy%%rest
Desk=#91#4%desk.example.com%3389%admin%0

[Bookmarks_1]
SubRep=Lab
Key box=#109#0%key.example.com%22%root%3%rest
";

    struct Sandbox {
        runtime: tokio::runtime::Runtime,
        backend: Backend,
        _scratch: tempfile::TempDir,
    }

    fn sandbox() -> Sandbox {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        let mut options = BackendOptions::sandbox(scratch.path().join("data"), &home);
        options.runtime = RuntimeChoice::Handle(runtime.handle().clone());
        let backend = runtime.block_on(Backend::start(options)).unwrap();
        Sandbox {
            runtime,
            backend,
            _scratch: scratch,
        }
    }

    fn review(storage: ImportedSshStorage, conflicts: ConflictStrategy) -> ImportReview {
        let mut review =
            ImportReview::from_content(ImportVendor::MobaXterm, MOBAXTERM, "MobaXterm.ini")
                .unwrap();
        review.ssh_storage = storage;
        review.conflicts = conflicts;
        review
    }

    #[test]
    fn imports_into_muxus_app_data_and_keeps_existing_hosts_on_a_second_run() {
        let sandbox = sandbox();
        let backend = &sandbox.backend;
        let first = sandbox
            .runtime
            .block_on(import_reviewed(
                backend,
                &review(ImportedSshStorage::Muxus, ConflictStrategy::Keep),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(first.added, 3);
        assert_eq!(
            ImportVendor::MobaXterm.import_complete_message(&first),
            "MobaXterm import complete — 3 added."
        );
        let saved = sandbox.runtime.block_on(backend.saved_hosts()).unwrap();
        let mut names: Vec<String> = saved.profiles.iter().map(|p| p.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["Desk", "Key box", "Prod SSH"]);
        let prod = saved
            .profiles
            .iter()
            .find(|profile| profile.name == "Prod SSH")
            .unwrap();
        assert_eq!(prod.metadata.group.as_deref(), Some("Production/Europe"));
        // ssh_config stays untouched with Muxus storage.
        let config = sandbox.runtime.block_on(backend.ssh_config()).unwrap();
        assert!(config.hosts.is_empty());

        let existing = sandbox.runtime.block_on(existing_hosts(backend)).unwrap();
        let again = review(ImportedSshStorage::Muxus, ConflictStrategy::Keep);
        assert_eq!(again.conflicting_count(&existing), 3);
        let kept = sandbox
            .runtime
            .block_on(import_reviewed(backend, &again))
            .unwrap()
            .unwrap();
        assert_eq!((kept.added, kept.updated, kept.skipped), (0, 0, 3));
        let replaced = sandbox
            .runtime
            .block_on(import_reviewed(
                backend,
                &review(ImportedSshStorage::Muxus, ConflictStrategy::Replace),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(replaced.updated, 3);
        assert_eq!(
            ImportVendor::MobaXterm.import_complete_message(&replaced),
            "MobaXterm import complete — 3 replaced."
        );
        sandbox.runtime.block_on(backend.shutdown());
    }

    #[test]
    fn writes_ssh_hosts_into_the_openssh_config_with_their_folders() {
        let sandbox = sandbox();
        let backend = &sandbox.backend;
        let mut review = review(ImportedSshStorage::Openssh, ConflictStrategy::Keep);
        // Only the SSH sessions.
        let desk = review
            .sessions()
            .iter()
            .find(|session| session.name() == "Desk")
            .unwrap()
            .id()
            .to_owned();
        review.toggle(&desk);
        assert_eq!(review.import_label(), "Import 2 sessions");
        let result = sandbox
            .runtime
            .block_on(import_reviewed(backend, &review))
            .unwrap()
            .unwrap();
        assert_eq!(result.added, 2);
        let config = sandbox.runtime.block_on(backend.ssh_config()).unwrap();
        let prod = config
            .hosts
            .iter()
            .find(|host| host.resolved.hostname == "prod.example.com")
            .expect("the Host block was written");
        assert_eq!(prod.resolved.port, 2222);
        assert_eq!(prod.resolved.user.as_deref(), Some("deploy"));
        assert_eq!(
            prod.metadata.as_ref().and_then(|m| m.group.as_deref()),
            Some("Production/Europe")
        );
        assert!(
            sandbox
                .runtime
                .block_on(backend.saved_hosts())
                .unwrap()
                .profiles
                .is_empty()
        );

        // Nothing selected: nothing to do.
        let mut none = review.clone();
        none.toggle_all();
        none.toggle_all();
        if none.all_selected() {
            none.toggle_all();
        }
        assert_eq!(
            sandbox
                .runtime
                .block_on(import_reviewed(backend, &none))
                .unwrap(),
            None
        );
        sandbox.runtime.block_on(backend.shutdown());
    }
}
