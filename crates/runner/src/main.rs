mod manifest;

use base64::Engine as _;
use frontend_forge_api::{
    FrontendIntegration, JSBundle, JsBundleNamespacedKeyRef, JsBundleRawFromSpec, JsBundleSpec,
    ManifestRenderError,
};
use frontend_forge_common::{
    ANNO_BUILD_JOB, ANNO_MANIFEST_HASH, CommonError, LABEL_FI_NAME, LABEL_MANAGED_BY,
    LABEL_MANIFEST_HASH, LABEL_SPEC_HASH, MANAGED_BY_VALUE, bounded_name,
    manifest_content_and_hash, serializable_hash,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Patch, PatchParams};
use kube::{Api, Client, Resource};
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::BTreeMap;
use std::env;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{error, info, warn};

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("missing env {key}: {source}"))]
    MissingEnv {
        key: &'static str,
        source: std::env::VarError,
    },
    #[snafu(display("invalid env {key}: {source}"))]
    InvalidEnv {
        key: &'static str,
        source: std::num::ParseIntError,
    },
    #[snafu(display("failed to initialize Kubernetes client in runner: {source}"))]
    KubeClientInit { source: kube::Error },
    #[snafu(display("failed to read FrontendIntegration {namespace}/{name}: {source}"))]
    GetFrontendIntegration {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to upsert bundle ConfigMap {namespace}/{name}: {source}"))]
    UpsertBundleConfigMap {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to upsert JSBundle {namespace}/{name}: {source}"))]
    UpsertJsBundle {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to render ExtensionManifest from FrontendIntegration: {source}"))]
    RenderManifest { source: ManifestRenderError },
    #[snafu(display("failed to canonicalize/hash runner manifest: {source}"))]
    ManifestHash { source: CommonError },
    #[snafu(display("failed to canonicalize/hash FrontendIntegration spec: {source}"))]
    SpecHash { source: CommonError },
    #[snafu(display(
        "failed to initialize build-service HTTP client (timeout={timeout_seconds}s): {source}"
    ))]
    BuildServiceClientInit {
        timeout_seconds: u64,
        source: reqwest::Error,
    },
    #[snafu(display("build-service request failed during {operation} {url}: {source}"))]
    BuildServiceRequest {
        operation: &'static str,
        url: String,
        source: reqwest::Error,
    },
    #[snafu(display("build-service returned non-success during {operation} {url}: {source}"))]
    BuildServiceResponseStatus {
        operation: &'static str,
        url: String,
        source: reqwest::Error,
    },
    #[snafu(display("failed to decode build-service response during {operation} {url}: {source}"))]
    BuildServiceDecode {
        operation: &'static str,
        url: String,
        source: reqwest::Error,
    },
    #[snafu(display("build-service returned failure: {message}"))]
    BuildFailed { message: String },
    #[snafu(display("failed to decode base64 artifact for {path}: {source}"))]
    DecodeArtifactBase64 {
        path: String,
        source: base64::DecodeError,
    },
    #[snafu(display("artifact {path} is not valid UTF-8 after decoding: {source}"))]
    ArtifactNotUtf8 {
        path: String,
        source: std::string::FromUtf8Error,
    },
    #[snafu(display("no suitable JS bundle artifact found (wanted key '{desired_key}')"))]
    MissingBundleArtifact { desired_key: String },
    #[snafu(display("fi status.observed_spec_hash not available within grace period"))]
    StaleCheckTimeout,
}

#[derive(Clone, Debug)]
struct RunnerConfig {
    work_namespace: String,
    fi_name: String,
    spec_hash: String,
    jsbundle_name: String,
    jsbundle_configmap_namespace: String,
    jsbundle_config_key: String,
    build_service_base_url: String,
    build_service_timeout_seconds: u64,
    stale_check_grace_seconds: u64,
    poll_interval_seconds: u64,
}

impl RunnerConfig {
    fn from_env() -> Result<Self, Error> {
        Ok(Self {
            work_namespace: env::var("WORK_NAMESPACE")
                .unwrap_or_else(|_| "extension-frontend-forge".to_string()),
            fi_name: required_env("FI_NAME")?,
            spec_hash: required_env_alias("SPEC_HASH", "MANIFEST_HASH")?,
            jsbundle_name: required_env("JSBUNDLE_NAME")?,
            jsbundle_configmap_namespace: env::var("JSBUNDLE_CONFIGMAP_NAMESPACE")
                .unwrap_or_else(|_| "extension-frontend-forge".to_string()),
            jsbundle_config_key: env::var("JSBUNDLE_CONFIG_KEY")
                .unwrap_or_else(|_| "index.js".to_string()),
            build_service_base_url: required_env("BUILD_SERVICE_BASE_URL")?,
            build_service_timeout_seconds: parse_env_u64("BUILD_SERVICE_TIMEOUT_SECONDS", 600)?,
            stale_check_grace_seconds: parse_env_u64("STALE_CHECK_GRACE_SECONDS", 30)?,
            poll_interval_seconds: parse_env_u64("BUILD_STATUS_POLL_SECONDS", 2)?,
        })
    }
}

fn required_env(key: &'static str) -> Result<String, Error> {
    env::var(key).context(MissingEnvSnafu { key })
}

fn required_env_alias(primary: &'static str, legacy: &'static str) -> Result<String, Error> {
    match env::var(primary) {
        Ok(v) => Ok(v),
        Err(_) => required_env(legacy),
    }
}

fn parse_env_u64(key: &'static str, default: u64) -> Result<u64, Error> {
    match env::var(key) {
        Ok(v) => v.parse::<u64>().context(InvalidEnvSnafu { key }),
        Err(_) => Ok(default),
    }
}

#[derive(Clone)]
struct BuildServiceClient {
    base_url: String,
    client: reqwest::Client,
    poll_interval: Duration,
}

#[derive(Debug, Serialize)]
struct CreateBuildRequest {
    #[serde(rename = "manifestHash")]
    manifest_hash: String,
    manifest: String,
    context: BuildContext,
}

#[derive(Debug, Serialize)]
struct BuildContext {
    namespace: String,
    #[serde(rename = "frontendIntegration")]
    frontend_integration: String,
}

#[derive(Debug, Deserialize)]
struct CreateBuildResponse {
    #[serde(rename = "buildId")]
    build_id: String,
    status: BuildState,
}

#[derive(Debug, Deserialize)]
struct BuildStatusResponse {
    #[serde(rename = "buildId")]
    _build_id: String,
    status: BuildState,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BuildFilesResponse {
    #[serde(rename = "buildId")]
    _build_id: String,
    files: Vec<RemoteFile>,
}

#[derive(Debug, Deserialize)]
struct RemoteFile {
    path: String,
    encoding: String,
    content: String,
    #[serde(default)]
    _sha256: Option<String>,
    #[serde(default)]
    _size: Option<u64>,
    #[serde(rename = "contentType")]
    #[serde(default)]
    _content_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum BuildState {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl BuildServiceClient {
    fn new(cfg: &RunnerConfig) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.build_service_timeout_seconds))
            .build()
            .context(BuildServiceClientInitSnafu {
                timeout_seconds: cfg.build_service_timeout_seconds,
            })?;
        Ok(Self {
            base_url: cfg.build_service_base_url.trim_end_matches('/').to_string(),
            client,
            poll_interval: Duration::from_secs(cfg.poll_interval_seconds),
        })
    }

    async fn create_build(
        &self,
        cfg: &RunnerConfig,
        manifest_hash: &str,
        manifest: &str,
    ) -> Result<CreateBuildResponse, Error> {
        let url = format!("{}/v1/builds", self.base_url);
        let req = CreateBuildRequest {
            manifest_hash: manifest_hash.to_string(),
            manifest: manifest.to_string(),
            context: BuildContext {
                namespace: cfg.work_namespace.clone(),
                frontend_integration: cfg.fi_name.clone(),
            },
        };

        let resp =
            self.client
                .post(&url)
                .json(&req)
                .send()
                .await
                .context(BuildServiceRequestSnafu {
                    operation: "create_build",
                    url: url.clone(),
                })?;
        let resp = resp
            .error_for_status()
            .context(BuildServiceResponseStatusSnafu {
                operation: "create_build",
                url: url.clone(),
            })?;
        resp.json().await.context(BuildServiceDecodeSnafu {
            operation: "create_build",
            url,
        })
    }

    async fn wait_for_completion(&self, build_id: &str) -> Result<(), Error> {
        loop {
            let status = self.get_status(build_id).await?;
            match status.status {
                BuildState::Pending | BuildState::Running => {
                    sleep(self.poll_interval).await;
                }
                BuildState::Succeeded => return Ok(()),
                BuildState::Failed => {
                    return Err(Error::BuildFailed {
                        message: status
                            .message
                            .unwrap_or_else(|| "build-service returned FAILED".to_string()),
                    });
                }
            }
        }
    }

    async fn get_status(&self, build_id: &str) -> Result<BuildStatusResponse, Error> {
        let url = format!("{}/v1/builds/{}", self.base_url, build_id);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context(BuildServiceRequestSnafu {
                operation: "get_build_status",
                url: url.clone(),
            })?;
        let resp = resp
            .error_for_status()
            .context(BuildServiceResponseStatusSnafu {
                operation: "get_build_status",
                url: url.clone(),
            })?;
        resp.json().await.context(BuildServiceDecodeSnafu {
            operation: "get_build_status",
            url,
        })
    }

    async fn fetch_files(&self, build_id: &str) -> Result<Vec<RemoteFile>, Error> {
        let url = format!("{}/v1/builds/{}/files", self.base_url, build_id);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context(BuildServiceRequestSnafu {
                operation: "get_build_files",
                url: url.clone(),
            })?;
        let resp = resp
            .error_for_status()
            .context(BuildServiceResponseStatusSnafu {
                operation: "get_build_files",
                url: url.clone(),
            })?;
        let payload: BuildFilesResponse = resp.json().await.context(BuildServiceDecodeSnafu {
            operation: "get_build_files",
            url,
        })?;
        Ok(payload.files)
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,frontend_forge_runner=debug".into()),
        )
        .init();

    match run().await {
        Ok(()) => Ok(()),
        Err(err) => {
            error!(error = %err, "runner failed");
            Err(err)
        }
    }
}

async fn run() -> Result<(), Error> {
    let cfg = RunnerConfig::from_env()?;
    let kube = Client::try_default().await.context(KubeClientInitSnafu)?;
    let fi_api = Api::<FrontendIntegration>::all(kube.clone());
    let fi_for_build =
        fi_api
            .get(&cfg.fi_name)
            .await
            .with_context(|_| GetFrontendIntegrationSnafu {
                namespace: "<cluster>".to_string(),
                name: cfg.fi_name.clone(),
            })?;
    let computed_spec_hash = serializable_hash(&fi_for_build.spec).context(SpecHashSnafu)?;
    if computed_spec_hash != cfg.spec_hash {
        warn!(
            fi = %cfg.fi_name,
            expected_spec_hash = %cfg.spec_hash,
            actual_spec_hash = %computed_spec_hash,
            "runner observed newer/different FI spec before build; skipping stale job"
        );
        return Ok(());
    }
    let manifest_value =
        manifest::render_extension_manifest(&fi_for_build).context(RenderManifestSnafu)?;
    let (manifest, manifest_hash) =
        manifest_content_and_hash(&manifest_value).context(ManifestHashSnafu)?;

    let build_client = BuildServiceClient::new(&cfg)?;

    info!(
        fi = %cfg.fi_name,
        spec_hash = %cfg.spec_hash,
        manifest_hash = %manifest_hash,
        "starting build runner"
    );
    let create = build_client
        .create_build(&cfg, &manifest_hash, &manifest)
        .await?;
    info!(build_id = %create.build_id, status = ?create.status, "build created");
    build_client.wait_for_completion(&create.build_id).await?;
    let files = build_client.fetch_files(&create.build_id).await?;
    info!(build_id = %create.build_id, files = files.len(), "build artifacts fetched");
    let fi = stale_check(&fi_api, &cfg).await?;
    if fi.is_none() {
        warn!("build became stale; exiting without writing JSBundle");
        return Ok(());
    }
    let fi = fi.expect("checked above");

    let (bundle_key, bundle_content) = select_bundle_artifact(&cfg, files)?;
    let configmap_name = bundle_configmap_name(&cfg.jsbundle_name);
    let configmap_api =
        Api::<ConfigMap>::namespaced(kube.clone(), &cfg.jsbundle_configmap_namespace);
    upsert_bundle_configmap(
        &configmap_api,
        &cfg,
        &fi,
        &configmap_name,
        &bundle_key,
        &bundle_content,
        &manifest_hash,
    )
    .await?;

    let bundle_api = Api::<JSBundle>::all(kube);
    upsert_jsbundle(
        &bundle_api,
        &cfg,
        &configmap_name,
        &bundle_key,
        &manifest_hash,
    )
    .await?;
    info!(bundle = %cfg.jsbundle_name, "jsbundle upserted");
    Ok(())
}

async fn stale_check(
    fi_api: &Api<FrontendIntegration>,
    cfg: &RunnerConfig,
) -> Result<Option<FrontendIntegration>, Error> {
    let deadline = Instant::now() + Duration::from_secs(cfg.stale_check_grace_seconds);

    loop {
        let fi = fi_api
            .get(&cfg.fi_name)
            .await
            .with_context(|_| GetFrontendIntegrationSnafu {
                namespace: "<cluster>".to_string(),
                name: cfg.fi_name.clone(),
            })?;
        let observed = fi
            .status
            .as_ref()
            .and_then(|s| s.observed_spec_hash.as_deref())
            .or_else(|| {
                fi.status
                    .as_ref()
                    .and_then(|s| s.observed_manifest_hash.as_deref())
            });

        match observed {
            Some(hash) if hash == cfg.spec_hash => return Ok(Some(fi)),
            Some(_) => return Ok(None),
            None if Instant::now() < deadline => {
                sleep(Duration::from_secs(2)).await;
            }
            None => return Err(Error::StaleCheckTimeout),
        }
    }
}

async fn upsert_bundle_configmap(
    configmap_api: &Api<ConfigMap>,
    cfg: &RunnerConfig,
    fi: &FrontendIntegration,
    configmap_name: &str,
    bundle_key: &str,
    bundle_content: &str,
    manifest_hash: &str,
) -> Result<(), Error> {
    let owner_refs = fi.controller_owner_ref(&()).map(|o| vec![o]);
    let mut labels = BTreeMap::new();
    labels.insert(LABEL_MANAGED_BY.to_string(), MANAGED_BY_VALUE.to_string());
    labels.insert(LABEL_FI_NAME.to_string(), cfg.fi_name.clone());
    labels.insert(
        LABEL_SPEC_HASH.to_string(),
        cfg.spec_hash
            .strip_prefix("sha256:")
            .unwrap_or(&cfg.spec_hash)
            .to_string(),
    );
    labels.insert(
        LABEL_MANIFEST_HASH.to_string(),
        manifest_hash
            .strip_prefix("sha256:")
            .unwrap_or(manifest_hash)
            .to_string(),
    );

    let mut annotations = BTreeMap::new();
    annotations.insert(ANNO_BUILD_JOB.to_string(), job_name_from_env());
    annotations.insert(ANNO_MANIFEST_HASH.to_string(), manifest_hash.to_string());

    let cm = ConfigMap {
        metadata: kube::core::ObjectMeta {
            name: Some(configmap_name.to_string()),
            namespace: Some(cfg.jsbundle_configmap_namespace.clone()),
            owner_references: owner_refs,
            labels: Some(labels),
            annotations: Some(annotations),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            bundle_key.to_string(),
            bundle_content.to_string(),
        )])),
        ..Default::default()
    };

    configmap_api
        .patch(
            configmap_name,
            &PatchParams::apply("frontend-forge-builder-runner").force(),
            &Patch::Apply(&cm),
        )
        .await
        .with_context(|_| UpsertBundleConfigMapSnafu {
            namespace: cfg.jsbundle_configmap_namespace.clone(),
            name: configmap_name.to_string(),
        })?;

    Ok(())
}

async fn upsert_jsbundle(
    bundle_api: &Api<JSBundle>,
    cfg: &RunnerConfig,
    configmap_name: &str,
    bundle_key: &str,
    manifest_hash: &str,
) -> Result<(), Error> {
    let mut labels = BTreeMap::new();
    labels.insert(LABEL_MANAGED_BY.to_string(), MANAGED_BY_VALUE.to_string());
    labels.insert(LABEL_FI_NAME.to_string(), cfg.fi_name.clone());
    labels.insert(
        LABEL_SPEC_HASH.to_string(),
        cfg.spec_hash
            .strip_prefix("sha256:")
            .unwrap_or(&cfg.spec_hash)
            .to_string(),
    );
    labels.insert(
        LABEL_MANIFEST_HASH.to_string(),
        manifest_hash
            .strip_prefix("sha256:")
            .unwrap_or(manifest_hash)
            .to_string(),
    );

    let mut annotations = BTreeMap::new();
    annotations.insert(ANNO_BUILD_JOB.to_string(), job_name_from_env());
    annotations.insert(ANNO_MANIFEST_HASH.to_string(), manifest_hash.to_string());

    let bundle = JSBundle {
        metadata: kube::core::ObjectMeta {
            name: Some(cfg.jsbundle_name.clone()),
            labels: Some(labels),
            annotations: Some(annotations),
            ..Default::default()
        },
        spec: JsBundleSpec {
            raw: None,
            raw_from: Some(JsBundleRawFromSpec {
                config_map_key_ref: Some(JsBundleNamespacedKeyRef {
                    key: bundle_key.to_string(),
                    name: configmap_name.to_string(),
                    namespace: cfg.jsbundle_configmap_namespace.clone(),
                    optional: None,
                }),
                secret_key_ref: None,
                url: None,
            }),
        },
        status: None,
    };

    bundle_api
        .patch(
            &cfg.jsbundle_name,
            &PatchParams::apply("frontend-forge-builder-runner").force(),
            &Patch::Apply(&bundle),
        )
        .await
        .with_context(|_| UpsertJsBundleSnafu {
            namespace: "<cluster>".to_string(),
            name: cfg.jsbundle_name.clone(),
        })?;

    Ok(())
}

fn bundle_configmap_name(jsbundle_name: &str) -> String {
    bounded_name(&format!("{}-config", jsbundle_name), 63)
}

fn select_bundle_artifact(
    cfg: &RunnerConfig,
    remote_files: Vec<RemoteFile>,
) -> Result<(String, String), Error> {
    let desired_key = cfg.jsbundle_config_key.clone();
    let selected_idx = remote_files
        .iter()
        .position(|f| f.path == desired_key)
        .or_else(|| {
            if remote_files.len() == 1 {
                Some(0)
            } else {
                remote_files.iter().position(|f| f.path.ends_with(".js"))
            }
        })
        .ok_or_else(|| Error::MissingBundleArtifact {
            desired_key: desired_key.clone(),
        })?;

    let file = remote_files
        .into_iter()
        .nth(selected_idx)
        .expect("selected index must exist");
    let content = decode_remote_file_to_utf8(&file)?;
    let key = if file.path.contains('/') {
        desired_key
    } else {
        file.path
    };
    Ok((key, content))
}

fn decode_remote_file_to_utf8(remote: &RemoteFile) -> Result<String, Error> {
    match remote.encoding.as_str() {
        "utf8" | "text" | "plain" => Ok(remote.content.clone()),
        "base64" => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(remote.content.as_bytes())
                .context(DecodeArtifactBase64Snafu {
                    path: remote.path.clone(),
                })?;
            String::from_utf8(bytes).context(ArtifactNotUtf8Snafu {
                path: remote.path.clone(),
            })
        }
        other => Err(Error::BuildFailed {
            message: format!(
                "unsupported artifact encoding '{}' for {}",
                other, remote.path
            ),
        }),
    }
}

fn job_name_from_env() -> String {
    env::var("HOSTNAME").unwrap_or_else(|_| "unknown-job".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_file() {
        let file = RemoteFile {
            path: "index.js".to_string(),
            encoding: "base64".to_string(),
            content: "Zm9v".to_string(),
            _sha256: Some("abc".to_string()),
            _size: Some(3),
            _content_type: Some("application/javascript".to_string()),
        };

        let decoded = decode_remote_file_to_utf8(&file).unwrap();
        assert_eq!(decoded, "foo");
    }

    #[test]
    fn rejects_unknown_encoding() {
        let file = RemoteFile {
            path: "index.js".to_string(),
            encoding: "gzip".to_string(),
            content: String::new(),
            _sha256: None,
            _size: None,
            _content_type: None,
        };

        assert!(decode_remote_file_to_utf8(&file).is_err());
    }
}
