use crate::model::Ledger;
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
        let ledger: Ledger = if ledger_path.exists() {
            serde_json::from_slice(&fs::read(ledger_path)?)?
        } else {
            Ledger::default()
        };
        ensure!(
            ledger.version == 1,
            "unsupported ledger version {}",
            ledger.version
        );
        for (id, job) in &ledger.jobs {
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
        Ok(Self {
            root,
            ledger,
            _lock: lock,
        })
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
