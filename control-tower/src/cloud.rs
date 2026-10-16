use crate::model::Job;
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use google_cloud_auth::credentials::Credentials;
use google_cloud_compute_v1::{
    client::{Instances, ZoneOperations},
    model::{self, Instance, Operation},
};
use google_cloud_gax::{
    error::{CredentialsError, Error, rpc::Code},
    retry_policy::NeverRetry,
};
use serde_json::json;
use std::{collections::HashMap, time::Duration};

/// Retry only failures whose declared contract permits another attempt. Unknown
/// responses, ownership violations and invalid input require operator action.
pub fn retryable(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(credentials) = cause.downcast_ref::<CredentialsError>() {
            return credentials.is_transient();
        }
    }
    for cause in error.chain() {
        if cause.is::<tokio::time::error::Elapsed>() {
            return true;
        }
        if let Some(error) = cause.downcast_ref::<Error>() {
            if error.is_timeout() {
                return true;
            }
            if let Some(status) = error.http_status_code() {
                return transient_status(status);
            }
            if let Some(status) = error.status() {
                return matches!(
                    status.code,
                    Code::Unavailable
                        | Code::DeadlineExceeded
                        | Code::ResourceExhausted
                        | Code::Aborted
                        | Code::Internal
                );
            }
            if error.is_deserialization() || error.is_serialization() {
                return false;
            }
            // With no service status or format error, the SDK failure may have
            // an uncertain network outcome. Keep its resource identity and retry.
            return true;
        }
        if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
            return error
                .status()
                .map(|status| transient_status(status.as_u16()))
                .unwrap_or(error.is_connect() || error.is_timeout());
        }
    }
    false
}

fn transient_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

// Friendzone and the operator CLI use direct HTTP; GCE uses the official SDK.
pub fn client(seconds: u64) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(seconds))
        .build()?)
}

pub async fn response_bytes(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "upstream response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// ADC and its token cache belong to the process. Reuse SDK connection pools
/// for jobs with the same endpoint and request timeout.
pub struct Clients {
    credentials: Credentials,
    cache: HashMap<(String, u64), (Instances, ZoneOperations)>,
}

impl Clients {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            cache: HashMap::new(),
        }
    }

    pub fn retain<'a>(&mut self, jobs: impl Iterator<Item = &'a Job>) {
        let live: std::collections::HashSet<_> = jobs
            .filter(|job| job.active())
            .map(|job| {
                (
                    &job.snapshot.cloud.endpoint,
                    job.snapshot.request_timeout_secs,
                )
            })
            .collect();
        self.cache
            .retain(|(endpoint, timeout), _| live.contains(&(endpoint, *timeout)));
    }

    pub async fn for_job(&mut self, job: &Job) -> Result<Api> {
        // Proxy routing and credential selection belong to the process
        // environment and take effect on server restart.
        let key = (
            job.snapshot.cloud.endpoint.clone(),
            job.snapshot.request_timeout_secs,
        );
        if !self.cache.contains_key(&key) {
            let timeout = Duration::from_secs(key.1);
            // The ledger owns retries and polling. A timeout must return control
            // rather than hide another RPC or wait for a cloud operation to finish.
            let (instances, operations) = tokio::time::timeout(timeout, async {
                let instances = Instances::builder()
                    .with_endpoint(&key.0)
                    .with_credentials(self.credentials.clone())
                    .with_retry_policy(NeverRetry)
                    .with_attempt_timeout(timeout)
                    .build()
                    .await?;
                let operations = ZoneOperations::builder()
                    .with_endpoint(&key.0)
                    .with_credentials(self.credentials.clone())
                    .with_retry_policy(NeverRetry)
                    .with_attempt_timeout(timeout)
                    .build()
                    .await?;
                Ok::<_, anyhow::Error>((instances, operations))
            })
            .await
            .context("GCE client initialization timed out")??;
            self.cache.insert(key.clone(), (instances, operations));
        }
        let (instances, operations) = self.cache[&key].clone();
        Ok(Api {
            instances,
            operations,
            project: job.snapshot.cloud.project.clone(),
            zone: job.snapshot.worker_profile.zone.clone(),
            timeout: Duration::from_secs(key.1),
        })
    }
}

pub struct Api {
    instances: Instances,
    operations: ZoneOperations,
    project: String,
    zone: String,
    timeout: Duration,
}

async fn optional<T>(
    timeout: Duration,
    request: impl std::future::Future<Output = google_cloud_compute_v1::Result<T>>,
) -> Result<Option<T>> {
    match tokio::time::timeout(timeout, request)
        .await
        .context("GCE request timed out")?
    {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.http_status_code() == Some(404) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn operation_name(operation: &Operation) -> Result<String> {
    let name = operation
        .name
        .as_deref()
        .context("GCE operation has no name")?;
    ensure!(
        !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "invalid GCE operation name"
    );
    Ok(name.to_owned())
}

fn operation_status(operation: &Operation) -> Result<&model::operation::Status> {
    let status = operation
        .status
        .as_ref()
        .context("GCE operation has no status")?;
    match status {
        model::operation::Status::Pending
        | model::operation::Status::Running
        | model::operation::Status::Done => Ok(status),
        status => bail!("unsupported GCE operation status: {status:?}"),
    }
}

pub fn operation_done(operation: &Operation) -> Result<bool> {
    match operation_status(operation)? {
        model::operation::Status::Pending | model::operation::Status::Running => Ok(false),
        model::operation::Status::Done => {
            if let Some(error) = &operation.error {
                bail!("GCE operation failed: {}", serde_json::to_string(error)?);
            }
            ensure!(
                operation.http_error_status_code.is_none(),
                "GCE operation failed: {:?}: {:?}",
                operation.http_error_status_code,
                operation.http_error_message
            );
            Ok(true)
        }
        status => bail!("unsupported GCE operation status: {status:?}"),
    }
}

impl Api {
    pub async fn get(&self, identity: &str) -> Result<Option<Instance>> {
        ensure!(
            crate::config::valid_name(identity)
                || !identity.is_empty() && identity.bytes().all(|c| c.is_ascii_digit()),
            "invalid instance identity"
        );
        optional(
            self.timeout,
            self.instances
                .get()
                .set_project(&self.project)
                .set_zone(&self.zone)
                .set_instance(identity)
                .send(),
        )
        .await
    }

    pub async fn operation(&self, name: &str) -> Result<Option<Operation>> {
        ensure!(
            !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "invalid GCE operation name"
        );
        let operation = optional(
            self.timeout,
            self.operations
                .get()
                .set_project(&self.project)
                .set_zone(&self.zone)
                .set_operation(name)
                .send(),
        )
        .await?;
        if let Some(operation) = &operation {
            operation_status(operation)?;
        }
        Ok(operation)
    }

    pub async fn insert(&self, job: &Job) -> Result<Operation> {
        let profile = &job.snapshot.worker_profile;
        let items = job
            .metadata()
            .into_iter()
            .map(|(key, value)| model::metadata::Items::new().set_key(key).set_value(value));
        let body = Instance::new()
            .set_name(&job.name)
            .set_machine_type(format!(
                "zones/{}/machineTypes/{}",
                profile.zone, profile.machine_type
            ))
            .set_disks([model::AttachedDisk::new()
                .set_boot(true)
                .set_auto_delete(true)
                .set_type(model::attached_disk::Type::Persistent)
                .set_initialize_params(
                    model::AttachedDiskInitializeParams::new()
                        .set_source_image(&profile.source_image)
                        .set_disk_size_gb(i64::from(profile.disk_gb)),
                )])
            .set_network_interfaces([model::NetworkInterface::new()
                .set_subnetwork(&profile.subnetwork)
                .set_stack_type(model::network_interface::StackType::Ipv4Only)])
            .set_can_ip_forward(false)
            .set_labels([("control-tower-job", job.submission.id.simple().to_string())])
            .set_metadata(model::Metadata::new().set_items(items))
            .set_tags(model::Tags::new().set_items(profile.network_tags.clone()))
            .set_scheduling(model::Scheduling::new().set_automatic_restart(false));
        // Empty service_accounts/access_configs are omitted by the SDK serializer.
        // GCE's REST contract gives omitted arrays the same no-account/no-NAT meaning.
        optional(
            self.timeout,
            self.instances
                .insert()
                .set_project(&self.project)
                .set_zone(&self.zone)
                .set_request_id(job.submission.id.to_string())
                .set_body(body)
                .send(),
        )
        .await?
        .context("GCE insert returned 404")
    }

    pub async fn delete(&self, job: &Job) -> Result<Option<Operation>> {
        let identity = job
            .instance_id
            .as_deref()
            .context("cannot delete an undiscovered instance")?;
        ensure!(
            !identity.is_empty() && identity.bytes().all(|c| c.is_ascii_digit()),
            "invalid GCE instance ID"
        );
        // Numeric instance IDs address an incarnation, not a reusable VM name.
        optional(
            self.timeout,
            self.instances
                .delete()
                .set_project(&self.project)
                .set_zone(&self.zone)
                .set_instance(identity)
                .send(),
        )
        .await
    }
}

pub async fn register(job: &Job) -> Result<()> {
    let response = client(job.snapshot.request_timeout_secs)?
        .post(format!(
            "{}/api/containers",
            job.snapshot.friendzone_url.trim_end_matches('/')
        ))
        .json(&json!({"name":job.name,"ip":job.ip.context("registration requires an IP")?}))
        .send()
        .await?;
    let response = response
        .error_for_status()
        .context("Friendzone registration")?;
    ensure!(
        response.status().is_success(),
        "Friendzone registration redirected: {}",
        response.status()
    );
    Ok(())
}

pub async fn unregister(job: &Job) -> Result<()> {
    let response = client(job.snapshot.request_timeout_secs)?
        .delete(format!(
            "{}/api/containers/{}",
            job.snapshot.friendzone_url.trim_end_matches('/'),
            job.name
        ))
        .send()
        .await?;
    let response = response.error_for_status().context("Friendzone removal")?;
    ensure!(
        response.status().is_success(),
        "Friendzone removal redirected: {}",
        response.status()
    );
    Ok(())
}
