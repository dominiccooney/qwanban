use crate::model::{Ledger, MAX_FAILURES, Phase};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub struct Store {
    pub root: PathBuf,
    pub ledger: Ledger,
    _lock: File,
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        let root = fs::canonicalize(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("lock"))?;
        lock.try_lock_exclusive()
            .context("another Control Tower owns this data directory")?;
        fs::create_dir_all(root.join("artifacts"))?;
        let ledger_path = root.join("jobs.json");
        let (ledger, migrated): (Ledger, bool) = if ledger_path.exists() {
            let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&ledger_path)?)?;
            let migrated = migrate(&mut value)?;
            (serde_json::from_value(value)?, migrated)
        } else {
            (Ledger::default(), false)
        };
        ensure!(
            ledger.version == 3,
            "unsupported ledger version {}",
            ledger.version
        );
        for (id, job) in &ledger.jobs {
            if let Phase::Retrying {
                phase, failures, ..
            }
            | Phase::Blocked { phase, failures } = &job.phase
            {
                ensure!(
                    (1..=MAX_FAILURES).contains(failures)
                        && matches!(
                            **phase,
                            Phase::Provisioning { .. }
                                | Phase::Registering
                                | Phase::Running
                                | Phase::Cleaning { .. }
                        ),
                    "invalid suspended job phase"
                );
            }
            ensure!(
                *id == job.submission.id && job.name == format!("ct-{}", id.simple()),
                "invalid job identity in ledger"
            );
            ensure!(
                !id.is_nil() && !ledger.retired_ids.contains(id),
                "job ID is nil or retired"
            );
            ensure!(
                job.instance_id
                    .as_ref()
                    .is_none_or(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit())),
                "invalid persisted GCE instance ID"
            );
            ensure!(
                !job.running() || job.instance_id.is_some() && job.ip.is_some(),
                "running job lacks worker identity"
            );
            ensure!(
                job.active() || job.outcome.is_some() && job.finished_at.is_some(),
                "finished job lacks outcome"
            );
        }
        for entry in fs::read_dir(root.join("artifacts"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".part") {
                fs::remove_file(entry.path())?;
            }
        }
        let mut store = Self {
            root,
            ledger,
            _lock: lock,
        };
        if migrated {
            store.commit(store.ledger.clone())?;
            eprintln!("migrated job ledger to version 3");
        }
        Ok(store)
    }

    pub fn commit(&mut self, next: Ledger) -> Result<()> {
        let path = self.root.join("jobs.json");
        let temp = self.root.join(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = private_file(&temp)?;
            file.write_all(&serde_json::to_vec(&next)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &path)?;
            #[cfg(unix)]
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result.context("commit job ledger")?;
        self.ledger = next;
        Ok(())
    }

    pub fn artifact_path(&self, id: Uuid) -> PathBuf {
        self.root.join("artifacts").join(format!("{id}.zst"))
    }
    pub fn upload_path(&self, id: Uuid) -> PathBuf {
        self.root.join("artifacts").join(format!("{id}.part"))
    }

    /// Maintenance, upload admission and operator deletion use the same retirement
    /// path so removed jobs always release their packet and retire their UUID.
    pub fn retire_job(&self, next: &mut Ledger, id: Uuid) -> Result<()> {
        let job = next.jobs.get(&id).context("unknown job")?;
        ensure!(
            !job.active(),
            "active job cannot be deleted before cleanup finishes"
        );
        if job.artifact.is_some() {
            match fs::remove_file(self.artifact_path(id)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        next.jobs.remove(&id);
        next.retired_ids.insert(id);
        Ok(())
    }

    /// Maintenance and upload admission use the same oldest-finished-first policy.
    /// Active jobs and upload reservations are never candidates for eviction.
    pub fn expire_artifacts(&self, next: &mut Ledger, budget: u64) -> Result<u64> {
        let mut artifacts: Vec<_> = next
            .jobs
            .iter()
            .filter_map(|(id, j)| j.artifact.as_ref().map(|a| (*id, a.clone())))
            .collect();
        artifacts.sort_by_key(|(id, a)| (a.at, *id));
        let mut total: u64 = artifacts.iter().map(|(_, a)| a.bytes).sum();
        for (id, artifact) in artifacts {
            let path = self.artifact_path(id);
            if !path.exists() || total > budget && !next.jobs[&id].active() {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                next.jobs.get_mut(&id).unwrap().artifact = None;
                total = total.saturating_sub(artifact.bytes);
            }
        }
        Ok(total)
    }
}

/// Preserve job/resource identities and callback URLs when upgrading retry
/// state. Version 1 also needs image-profile and credential-source conversion.
fn migrate(value: &mut serde_json::Value) -> Result<bool> {
    use serde_json::Value;
    match value["version"].as_u64() {
        Some(3) => return Ok(false),
        Some(2) => {
            value["version"] = Value::from(3);
            return Ok(true);
        }
        Some(1) => {}
        version => anyhow::bail!("unsupported ledger version {version:?}"),
    }
    let jobs = value["jobs"]
        .as_object_mut()
        .context("invalid ledger jobs")?;
    for job in jobs.values_mut() {
        let job = job.as_object_mut().context("invalid legacy job")?;
        for (section, old, new) in [
            ("submission", "image", "worker_profile"),
            ("snapshot", "image", "worker_profile"),
        ] {
            let section = job
                .get_mut(section)
                .context("missing legacy job section")?
                .as_object_mut()
                .context("invalid legacy job section")?;
            ensure!(!section.contains_key(new), "mixed ledger versions");
            let old_value = section
                .remove(old)
                .context("legacy job has no image profile")?;
            section.insert(new.into(), old_value);
        }
        let cloud = job
            .get_mut("snapshot")
            .and_then(|snapshot| snapshot.get_mut("cloud"))
            .context("missing legacy cloud snapshot")?
            .as_object_mut()
            .context("invalid legacy cloud snapshot")?;
        let api_url = cloud
            .remove("api_url")
            .context("legacy snapshot has no API URL")?;
        let url = api_url.as_str().context("invalid legacy API URL")?;
        let mut endpoint = crate::config::http_url(url)?;
        ensure!(
            matches!(endpoint.path().trim_end_matches('/'), "" | "/compute/v1"),
            "legacy GCE endpoint has an unsupported path; ledger not migrated"
        );
        endpoint.set_path("");
        ensure!(
            !cloud.contains_key("endpoint"),
            "mixed cloud snapshot versions"
        );
        cloud.insert(
            "endpoint".into(),
            Value::String(endpoint.as_str().trim_end_matches('/').into()),
        );
        let auth = cloud
            .remove("auth")
            .context("legacy snapshot has no auth configuration")?;
        let auth = auth
            .as_object()
            .context("invalid legacy auth configuration")?;
        match auth.get("kind").and_then(Value::as_str) {
            Some("metadata") => ensure!(
                auth.len() == 2 && auth.get("url").is_some_and(Value::is_string),
                "invalid legacy metadata auth"
            ),
            Some("command") => ensure!(
                auth.len() == 3
                    && auth.get("program").is_some_and(Value::is_string)
                    && auth
                        .get("args")
                        .and_then(Value::as_array)
                        .is_some_and(|args| args.iter().all(Value::is_string)),
                "invalid legacy command auth"
            ),
            _ => anyhow::bail!("unknown legacy auth configuration"),
        }
    }
    value["version"] = Value::from(3);
    Ok(true)
}
