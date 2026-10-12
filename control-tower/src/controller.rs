use crate::{
    cloud::{self, Clients},
    config::{Config, validate_metadata},
    model::{Artifact, Job, Ledger, Outcome, Phase, Snapshot, Submission, now},
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use google_cloud_compute_v1::model::{Instance, instance::Status as InstanceStatus};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, Notify, RwLock, watch};
use uuid::Uuid;

pub struct App {
    pub store: Mutex<Store>,
    pub config: RwLock<Arc<Config>>,
    pub changed: watch::Sender<Arc<Ledger>>,
    pub wake: Notify,
    pub uploads: std::sync::Mutex<HashMap<Uuid, u64>>,
    pub relay_count: std::sync::atomic::AtomicUsize,
    pub reload_error: RwLock<Option<String>>,
    cloud: Mutex<Clients>,
}

impl App {
    pub fn open(
        config: Config,
        credentials: google_cloud_auth::credentials::Credentials,
    ) -> Result<Arc<Self>> {
        config.validate()?;
        let store = Store::open(&config.data_dir)?;
        let (changed, _) = watch::channel(Arc::new(store.ledger.clone()));
        Ok(Arc::new(Self {
            store: Mutex::new(store),
            config: RwLock::new(Arc::new(config)),
            changed,
            wake: Notify::new(),
            uploads: Default::default(),
            relay_count: Default::default(),
            reload_error: RwLock::new(None),
            cloud: Mutex::new(Clients::new(credentials)),
        }))
    }
    pub fn publish(&self, store: &Store) {
        self.changed.send_replace(Arc::new(store.ledger.clone()));
    }

    pub async fn submit(&self, submission: Submission) -> Result<Job> {
        ensure!(!submission.id.is_nil(), "submission ID must not be nil");
        ensure!(
            !submission.prompt.is_empty() || !submission.setup_script.is_empty(),
            "prompt or setup script is required"
        );
        ensure!(
            !submission.title.trim().is_empty()
                && submission.title.len() <= 512
                && submission.source.len() <= 512,
            "title/source must fit 512 bytes; title is required"
        );
        let mut store = self.store.lock().await;
        ensure!(
            !store.ledger.retired_ids.contains(&submission.id),
            "submission ID belongs to an expired job; use a new ID"
        );
        if let Some(job) = store.ledger.jobs.get(&submission.id) {
            ensure!(
                job.submission == submission,
                "submission ID already has different contents"
            );
            return Ok(job.clone());
        }
        let config = self.config.read().await.clone();
        ensure!(
            store.ledger.jobs.values().filter(|j| j.active()).count() < config.limits.max_jobs,
            "active job limit reached"
        );
        let worker_profile = config
            .worker_profiles
            .get(&submission.worker_profile)
            .context("unknown worker profile")?
            .clone();
        let job = Job {
            name: format!("ct-{}", submission.id.simple()),
            submission,
            snapshot: Snapshot {
                cloud: config.cloud.clone(),
                worker_profile,
                friendzone_url: config.friendzone_url.clone(),
                worker_url: config.worker_url(),
                request_timeout_secs: config.limits.request_timeout_secs,
                job_timeout_secs: config.limits.job_timeout_secs,
            },
            created_at: now(),
            updated_at: now(),
            finished_at: None,
            instance_id: None,
            ip: None,
            phase: Phase::Queued,
            outcome: None,
            last_error: None,
            artifact: None,
        };
        let metadata = job.metadata();
        for (key, value) in &metadata {
            validate_metadata(key, value)?;
        }
        ensure!(
            metadata
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                < 512 * 1024,
            "GCE metadata exceeds 512 KiB"
        );
        let mut next = store.ledger.clone();
        next.jobs.insert(job.submission.id, job.clone());
        store.commit(next)?;
        self.publish(&store);
        self.wake.notify_one();
        Ok(job)
    }

    pub async fn finish(
        &self,
        id: Uuid,
        peer: Option<std::net::IpAddr>,
        outcome: Outcome,
    ) -> Result<Job> {
        let mut store = self.store.lock().await;
        let mut next = store.ledger.clone();
        let job = next.jobs.get_mut(&id).context("unknown job")?;
        if let Some(peer) = peer {
            ensure!(job.ip == Some(peer), "source IP does not own this job");
            ensure!(
                !self.uploads.lock().unwrap().contains_key(&id),
                "artifact upload still in progress"
            );
        }
        if job.outcome.is_some() {
            ensure!(
                job.outcome == Some(outcome),
                "job already has another outcome"
            );
            return Ok(job.clone());
        }
        if peer.is_some() {
            ensure!(job.running(), "job is not running");
        }
        // Keep an in-flight provisioning operation until discovery settles its outcome.
        job.outcome = Some(outcome);
        job.updated_at = now();
        if matches!(job.phase, Phase::Queued) {
            job.phase = Phase::Finished;
            job.finished_at = Some(now());
        } else if !matches!(job.phase, Phase::Provisioning { .. }) {
            begin_cleanup(job);
        }
        let result = job.clone();
        store.commit(next)?;
        self.publish(&store);
        self.wake.notify_one();
        Ok(result)
    }

    pub async fn tick(&self) -> Result<()> {
        let ids: Vec<Uuid> = self
            .changed
            .borrow()
            .jobs
            .values()
            .filter(|j| j.active())
            .map(|j| j.submission.id)
            .collect();
        for id in ids {
            // All lifecycle reads, external writes and ledger commits are serialized
            // by this lock. Upload bodies and observatory streams never hold it.
            let mut store = self.store.lock().await;
            let mut job = store.ledger.jobs[&id].clone();
            if !job.active() {
                continue;
            }
            if job.outcome.is_none()
                && now().saturating_sub(job.created_at) >= job.snapshot.job_timeout_secs
            {
                job.outcome = Some(Outcome::TimedOut);
                if matches!(job.phase, Phase::Queued) {
                    job.phase = Phase::Finished;
                    job.finished_at = Some(now());
                } else if !matches!(job.phase, Phase::Provisioning { .. }) {
                    begin_cleanup(&mut job);
                }
            }
            let result = advance(&mut job, &mut *self.cloud.lock().await).await;
            match result {
                Ok(()) => job.last_error = None,
                Err(error) => {
                    eprintln!("job {id}: {error:#}");
                    job.last_error = Some(format!("{error:#}").chars().take(4096).collect());
                }
            }
            job.updated_at = now();
            let mut next = store.ledger.clone();
            if matches!(job.phase, Phase::Registering) {
                for other in next.jobs.values_mut() {
                    if other.submission.id != id && other.running() && other.ip == job.ip {
                        other.outcome = Some(Outcome::Lost);
                        begin_cleanup(other);
                        eprintln!("job {} lost its IP to {id}", other.submission.id);
                    }
                }
            }
            next.jobs.insert(id, job);
            store.commit(next)?;
            self.publish(&store);
        }
        self.retain().await
    }

    pub async fn retain(&self) -> Result<()> {
        let mut store = self.store.lock().await;
        let config = self.config.read().await.clone();
        let mut next = store.ledger.clone();
        // Recover committed files before calculating the retention budget.
        for entry in std::fs::read_dir(store.root.join("artifacts"))? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|ext| ext == "zst") {
                let id = entry
                    .path()
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.parse::<Uuid>().ok());
                if let Some(job) = id.and_then(|id| next.jobs.get_mut(&id)) {
                    if job.artifact.is_none() {
                        job.artifact = Some(Artifact {
                            bytes: entry.metadata()?.len(),
                            at: now(),
                        });
                    }
                } else {
                    std::fs::remove_file(entry.path())?;
                }
            }
        }
        store.expire_artifacts(&mut next, config.limits.artifact_budget_bytes)?;
        let mut finished: Vec<_> = next
            .jobs
            .values()
            .filter(|j| !j.active())
            .map(|j| (j.submission.id, j.finished_at))
            .collect();
        finished.sort_by_key(|(_, at)| *at);
        let remove = next
            .jobs
            .values()
            .filter(|j| !j.active())
            .count()
            .saturating_sub(config.limits.max_finished_jobs);
        for (id, _) in finished.into_iter().take(remove) {
            if next.jobs[&id].artifact.is_some() {
                match std::fs::remove_file(store.artifact_path(id)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            next.jobs.remove(&id);
            next.retired_ids.insert(id);
        }
        self.cloud.lock().await.retain(next.jobs.values());
        store.commit(next)?;
        self.publish(&store);
        Ok(())
    }

    pub async fn run(
        self: Arc<Self>,
        config_path: PathBuf,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let mut reload = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut last_bytes = std::fs::read(&config_path)?;
        let mut reload_rejected = false;
        let mut next_tick = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = self.wake.notified() => next_tick=tokio::time::Instant::now(),
                _ = reload.tick() => {
                    match std::fs::read(&config_path) {
                        Ok(bytes) if bytes!=last_bytes => {
                            last_bytes=bytes;
                            let result=Config::load(&config_path);
                            reload_rejected = true;
                            match result {
                                Ok(next)=>{
                                    let mut current=self.config.write().await;
                                    if current.restart_fields_match(&next) {
                                        // Provisioning settings change at submission; limits
                                        // change at the next request or maintenance pass.
                                        *current=Arc::new(next);reload_rejected=false;*self.reload_error.write().await=None;eprintln!("configuration reloaded");
                                    } else {*self.reload_error.write().await=Some("listener/data/UI directory changes require restart; reload rejected".into());}
                                },
                                Err(e)=>{let error=format!("{e:#}");eprintln!("reload rejected: {error}");*self.reload_error.write().await=Some(error);}
                            }
                        }
                        Err(e)=>*self.reload_error.write().await=Some(e.to_string()),
                        Ok(_) if !reload_rejected=>{*self.reload_error.write().await=None;}
                        Ok(_)=>{}
                    }
                    if tokio::time::Instant::now()>=next_tick {
                        self.tick().await?;
                        next_tick=tokio::time::Instant::now()+std::time::Duration::from_secs(self.config.read().await.limits.reconcile_secs);
                    }
                }
            }
        }
    }
}

fn begin_cleanup(job: &mut Job) {
    job.phase = Phase::Cleaning {
        unregister: true,
        delete: true,
        operation: None,
    };
}

fn owned(job: &Job, instance: &Instance) -> Result<()> {
    ensure!(
        instance.labels.get("control-tower-job") == Some(&job.submission.id.simple().to_string()),
        "VM name belongs to another job; refusing adoption"
    );
    ensure!(instance.id.is_some_and(|id| id > 0), "invalid instance ID");
    ensure!(
        instance.network_interfaces.len() == 1
            && instance.network_interfaces[0].access_configs.is_empty()
            && instance.network_interfaces[0]
                .ipv_6_access_configs
                .is_empty(),
        "worker must have one private-only NIC"
    );
    ensure!(
        instance.service_accounts.is_empty() && instance.can_ip_forward != Some(true),
        "worker must have no service account or IP forwarding"
    );
    match instance
        .status
        .as_ref()
        .context("GCE instance has no status")?
    {
        InstanceStatus::Deprovisioning
        | InstanceStatus::Pending
        | InstanceStatus::PendingStop
        | InstanceStatus::Provisioning
        | InstanceStatus::Repairing
        | InstanceStatus::Running
        | InstanceStatus::Staging
        | InstanceStatus::Stopped
        | InstanceStatus::Stopping
        | InstanceStatus::Suspended
        | InstanceStatus::Suspending
        | InstanceStatus::Terminated => {}
        status => anyhow::bail!("unsupported GCE instance status: {status:?}"),
    }
    Ok(())
}

fn instance_ip(instance: &Instance) -> Result<Option<std::net::IpAddr>> {
    instance.network_interfaces[0]
        .network_ip
        .as_deref()
        .map(str::parse)
        .transpose()
        .context("invalid worker IP")
}

async fn advance(job: &mut Job, clients: &mut Clients) -> Result<()> {
    match job.phase.clone() {
        // Persist the insertion intent in its own pass before any GCE request.
        // Cancellation of a queued job therefore never creates a VM.
        Phase::Queued => job.phase = Phase::Provisioning { operation: None },
        Phase::Provisioning { operation } => {
            let api = clients.for_job(job).await?;
            if let Some(ref operation) = operation
                && let Some(op) = api.operation(operation).await?
            {
                match cloud::operation_done(&op) {
                    Ok(false) => return Ok(()),
                    Err(e) => {
                        job.outcome.get_or_insert(Outcome::Failed);
                        begin_cleanup(job);
                        return Err(e);
                    }
                    Ok(true) => {}
                }
            }
            if let Some(instance) = api.get(&job.name).await? {
                owned(job, &instance)?;
                ensure!(
                    job.instance_id
                        .as_ref()
                        .is_none_or(|id| instance.id.is_some_and(|value| id == &value.to_string())),
                    "VM incarnation changed; refusing adoption"
                );
                job.instance_id = Some(instance.id.context("GCE instance has no ID")?.to_string());
                job.ip = instance_ip(&instance)?;
                if job.outcome.is_some() {
                    begin_cleanup(job);
                    return Ok(());
                }
                if dead(instance.status.as_ref().unwrap()) {
                    job.outcome = Some(Outcome::Lost);
                    begin_cleanup(job);
                } else if matches!(instance.status, Some(InstanceStatus::Running))
                    && job.ip.is_some()
                {
                    job.phase = Phase::Registering;
                }
            } else if operation.is_some() || job.instance_id.is_some() {
                job.outcome.get_or_insert(Outcome::Lost);
                begin_cleanup(job);
            } else if job.outcome.is_some() {
                // Without a known operation, retry the same insertion to settle an
                // uncertain earlier request before attempting cleanup.
                let op = api.insert(job).await?;
                job.phase = Phase::Provisioning {
                    operation: Some(cloud::operation_name(&op)?),
                };
            } else {
                let op = api.insert(job).await?;
                job.phase = Phase::Provisioning {
                    operation: Some(cloud::operation_name(&op)?),
                };
            }
        }
        Phase::Registering => {
            let api = clients.for_job(job).await?;
            let instance = api
                .get(
                    job.instance_id
                        .as_deref()
                        .context("registering job has no instance ID")?,
                )
                .await?;
            let Some(instance) = instance else {
                job.outcome = Some(Outcome::Lost);
                begin_cleanup(job);
                return Ok(());
            };
            owned(job, &instance)?;
            if instance_ip(&instance)? != job.ip || dead(instance.status.as_ref().unwrap()) {
                job.outcome = Some(Outcome::Lost);
                begin_cleanup(job);
                return Ok(());
            }
            cloud::register(job).await?;
            job.phase = Phase::Running;
        }
        Phase::Running => {
            let api = clients.for_job(job).await?;
            let instance = api
                .get(
                    job.instance_id
                        .as_deref()
                        .context("running job has no instance ID")?,
                )
                .await?;
            let dead = match instance {
                None => true,
                Some(instance) => {
                    owned(job, &instance)?;
                    ensure!(instance_ip(&instance)? == job.ip, "worker IP changed");
                    dead(instance.status.as_ref().unwrap())
                }
            };
            if dead {
                eprintln!("job {} lost its worker", job.submission.id);
                job.outcome = Some(Outcome::Lost);
                begin_cleanup(job);
            }
        }
        Phase::Cleaning {
            mut unregister,
            mut delete,
            mut operation,
        } => {
            let mut error = None;
            if unregister {
                match cloud::unregister(job).await {
                    Ok(()) => unregister = false,
                    Err(e) => error = Some(e),
                }
            }
            if delete {
                let result = async {
                    let api = clients.for_job(job).await?;
                    if job.instance_id.is_none() {
                        if let Some(instance) = api.get(&job.name).await? {
                            owned(job, &instance)?;
                            job.instance_id =
                                Some(instance.id.context("GCE instance has no ID")?.to_string());
                        } else {
                            return Ok(true);
                        }
                    }
                    if let Some(name) = &operation {
                        if let Some(op) = api.operation(name).await? {
                            match cloud::operation_done(&op) {
                                Ok(false) => return Ok(false),
                                Err(error) => {
                                    operation = None;
                                    return Err(error);
                                }
                                Ok(true) => {}
                            }
                        }
                        if api
                            .get(job.instance_id.as_deref().unwrap())
                            .await?
                            .is_none()
                        {
                            return Ok(true);
                        }
                    }
                    match api.delete(job).await? {
                        None => Ok(true),
                        Some(op) => {
                            let done = cloud::operation_done(&op)?;
                            operation = Some(cloud::operation_name(&op)?);
                            Ok(done)
                        }
                    }
                }
                .await;
                match result {
                    Ok(done) => delete = !done,
                    Err(e) => error = Some(e),
                }
            }
            job.phase = if !unregister && !delete {
                job.finished_at = Some(now());
                Phase::Finished
            } else {
                Phase::Cleaning {
                    unregister,
                    delete,
                    operation,
                }
            };
            if let Some(e) = error {
                return Err(e);
            }
        }
        Phase::Finished => {}
    }
    Ok(())
}

fn dead(status: &InstanceStatus) -> bool {
    matches!(
        status,
        InstanceStatus::Stopped
            | InstanceStatus::Terminated
            | InstanceStatus::Stopping
            | InstanceStatus::Suspended
            | InstanceStatus::Suspending
            | InstanceStatus::Deprovisioning
    )
}
