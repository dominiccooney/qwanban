use crate::{config::Auth, model::Job};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

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

pub struct Api {
    client: reqwest::Client,
    token: String,
    base: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstanceStatus {
    Deprovisioning,
    Pending,
    PendingStop,
    Provisioning,
    Repairing,
    Running,
    Staging,
    Stopped,
    Stopping,
    Suspended,
    Suspending,
    Terminated,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    pub id: String,
    pub status: InstanceStatus,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub network_interfaces: Vec<Nic>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Nic {
    #[serde(rename = "networkIP")]
    pub network_ip: Option<std::net::IpAddr>,
    #[serde(default)]
    pub access_configs: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationStatus {
    Pending,
    Running,
    Done,
}

#[derive(Debug, Deserialize)]
pub struct Operation {
    pub name: String,
    pub status: OperationStatus,
    pub error: Option<Value>,
}

impl Operation {
    pub fn done(&self) -> Result<bool> {
        if matches!(self.status, OperationStatus::Done) {
            if let Some(error) = &self.error {
                bail!("GCE operation failed: {error}");
            }
            return Ok(true);
        }
        Ok(false)
    }
}

impl Api {
    pub async fn new(job: &Job) -> Result<Self> {
        let client = client(job.snapshot.request_timeout_secs)?;
        let timeout = Duration::from_secs(job.snapshot.request_timeout_secs);
        let token = match &job.snapshot.cloud.auth {
            Auth::Metadata { url } => {
                let response = client
                    .get(url)
                    .header("Metadata-Flavor", "Google")
                    .send()
                    .await?;
                ensure!(
                    response.status().is_success(),
                    "metadata token request: {}",
                    response.status()
                );
                #[derive(Deserialize)]
                struct Token {
                    access_token: String,
                }
                serde_json::from_slice::<Token>(&response_bytes(response, 65536).await?)?
                    .access_token
            }
            Auth::Command { program, args } => {
                let output = tokio::time::timeout(
                    timeout,
                    tokio::process::Command::new(program)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .kill_on_drop(true)
                        .output(),
                )
                .await??;
                ensure!(
                    output.status.success(),
                    "token command exited {}",
                    output.status
                );
                ensure!(
                    output.stdout.len() <= 65536,
                    "token command output too large"
                );
                String::from_utf8(output.stdout)?.trim().to_owned()
            }
        };
        ensure!(!token.is_empty(), "empty cloud access token");
        let base = format!(
            "{}/projects/{}/zones/{}",
            job.snapshot.cloud.api_url.trim_end_matches('/'),
            job.snapshot.cloud.project,
            job.snapshot.image.zone
        );
        Ok(Self {
            client,
            token,
            base,
        })
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        request_id: Option<uuid::Uuid>,
    ) -> Result<Option<T>> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(id) = request_id {
            request = request.query(&[("requestId", id.to_string())]);
        }
        let response = request.send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = response_bytes(response, 2 * 1024 * 1024).await?;
        ensure!(
            status.is_success(),
            "GCE {path}: {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        Ok(Some(
            serde_json::from_slice(&bytes).context("decode GCE response")?,
        ))
    }

    pub async fn get(&self, identity: &str) -> Result<Option<Instance>> {
        ensure!(
            crate::config::valid_name(identity)
                || !identity.is_empty() && identity.bytes().all(|c| c.is_ascii_digit()),
            "invalid instance identity"
        );
        self.send(
            reqwest::Method::GET,
            &format!("/instances/{identity}"),
            None,
            None,
        )
        .await
    }
    pub async fn operation(&self, name: &str) -> Result<Option<Operation>> {
        ensure!(
            name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "invalid GCE operation name"
        );
        self.send(
            reqwest::Method::GET,
            &format!("/operations/{name}"),
            None,
            None,
        )
        .await
    }
    pub async fn insert(&self, job: &Job) -> Result<Operation> {
        let image = &job.snapshot.image;
        let items: Vec<Value> = job
            .metadata()
            .into_iter()
            .map(|(key, value)| json!({"key":key,"value":value}))
            .collect();
        let body = json!({
            "name":job.name,
            "machineType":format!("zones/{}/machineTypes/{}",image.zone,image.machine_type),
            "disks":[{"boot":true,"autoDelete":true,"type":"PERSISTENT","initializeParams":{"sourceImage":image.source_image,"diskSizeGb":image.disk_gb.to_string()}}],
            "networkInterfaces":[{"subnetwork":image.subnetwork,"accessConfigs":[],"stackType":"IPV4_ONLY"}],
            "serviceAccounts":[],"canIpForward":false,
            "labels":{"control-tower-job":job.submission.id.simple().to_string()},
            "metadata":{"items":items},"tags":{"items":image.network_tags},
            "scheduling":{"automaticRestart":false}
        });
        self.send(
            reqwest::Method::POST,
            "/instances",
            Some(body),
            Some(job.submission.id),
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
        self.send(
            reqwest::Method::DELETE,
            &format!("/instances/{identity}"),
            None,
            None,
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
    ensure!(
        response.status().is_success(),
        "Friendzone registration: {}",
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
    ensure!(
        response.status().is_success(),
        "Friendzone removal: {}",
        response.status()
    );
    Ok(())
}
