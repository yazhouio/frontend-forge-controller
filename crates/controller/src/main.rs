use chrono::Utc;
use frontend_forge_api::{
    ActiveBuildStatus, FrontendIntegration, FrontendIntegrationPhase, FrontendIntegrationStatus,
    JSBundle, ResourceRef,
};
use frontend_forge_common::{
    ANNO_OBSERVED_GENERATION, BUILD_KIND_VALUE, CommonError, DEFAULT_MANIFEST_FILENAME,
    DEFAULT_MANIFEST_MOUNT_PATH, LABEL_BUILD_KIND, LABEL_FI_NAME, LABEL_MANAGED_BY,
    LABEL_MANIFEST_HASH, MANAGED_BY_VALUE, MAX_SECRET_PAYLOAD_BYTES, default_bundle_name, job_name,
    manifest_content_and_hash, secret_name, time_nonce,
};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::JobStatus;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, Secret, SecretVolumeSource, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{ListParams, Patch, PatchParams, PostParams};
use kube::{Api, Client, Resource, ResourceExt};
use kube_runtime::controller::{Action, Controller};
use kube_runtime::watcher;
use serde_json::json;
use snafu::{ResultExt, Snafu};
use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("manifest/hash error: {source}"))]
    Common { source: CommonError },
    #[snafu(display("missing namespace for FrontendIntegration {name}"))]
    MissingNamespace { name: String },
    #[snafu(display("manifest payload exceeds secret size limit: {bytes} bytes"))]
    ManifestTooLarge { bytes: usize },
    #[snafu(display("failed to initialize Kubernetes client: {source}"))]
    KubeClientInit { source: kube::Error },
    #[snafu(display("failed to patch FrontendIntegration status {namespace}/{name}: {source}"))]
    PatchFrontendIntegrationStatus {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display(
        "failed to list Jobs in {namespace} for FrontendIntegration {fi_name} and manifestHash {manifest_hash}: {source}"
    ))]
    ListJobsForHash {
        namespace: String,
        fi_name: String,
        manifest_hash: String,
        source: kube::Error,
    },
    #[snafu(display("failed to get JSBundle {namespace}/{name}: {source}"))]
    GetJsBundle {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to create Job {namespace}/{name}: {source}"))]
    CreateJob {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to get existing Job after conflict {namespace}/{name}: {source}"))]
    GetJobAfterConflict {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to create Secret {namespace}/{name}: {source}"))]
    CreateSecret {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display("failed to get existing Secret after conflict {namespace}/{name}: {source}"))]
    GetSecretAfterConflict {
        namespace: String,
        name: String,
        source: kube::Error,
    },
}

#[derive(Clone, Debug)]
struct ControllerConfig {
    runner_image: String,
    runner_service_account: Option<String>,
    build_service_base_url: String,
    build_service_timeout_seconds: u64,
    stale_check_grace_seconds: u64,
    reconcile_requeue_seconds: u64,
    job_ttl_seconds_after_finished: Option<i32>,
}

impl ControllerConfig {
    fn from_env() -> Self {
        Self {
            runner_image: env::var("RUNNER_IMAGE")
                .unwrap_or_else(|_| "ghcr.io/example/frontend-forge-runner:latest".to_string()),
            runner_service_account: env::var("RUNNER_SERVICE_ACCOUNT").ok(),
            build_service_base_url: env::var("BUILD_SERVICE_BASE_URL")
                .unwrap_or_else(|_| "http://build-service.default.svc.cluster.local".to_string()),
            build_service_timeout_seconds: env::var("BUILD_SERVICE_TIMEOUT_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(600),
            stale_check_grace_seconds: env::var("STALE_CHECK_GRACE_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            reconcile_requeue_seconds: env::var("RECONCILE_REQUEUE_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            job_ttl_seconds_after_finished: env::var("JOB_TTL_SECONDS_AFTER_FINISHED")
                .ok()
                .and_then(|v| v.parse().ok()),
        }
    }
}

#[derive(Clone)]
struct ContextData {
    client: Client,
    config: ControllerConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedJobPhase {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,frontend_forge_controller=debug".into()),
        )
        .init();

    let client = Client::try_default().await.context(KubeClientInitSnafu)?;
    let ctx = Arc::new(ContextData {
        client: client.clone(),
        config: ControllerConfig::from_env(),
    });

    let fi_api = Api::<FrontendIntegration>::all(client.clone());
    let job_api = Api::<Job>::all(client.clone());
    let bundle_api = Api::<JSBundle>::all(client.clone());

    Controller::new(fi_api, watcher::Config::default())
        .owns(job_api, watcher::Config::default())
        .owns(bundle_api, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|result| async move {
            match result {
                Ok((obj_ref, action)) => info!(?obj_ref, ?action, "reconciled"),
                Err(err) => error!(error = %err, "controller reconcile stream error"),
            }
        })
        .await;

    Ok(())
}

fn error_policy(_fi: Arc<FrontendIntegration>, err: &Error, _ctx: Arc<ContextData>) -> Action {
    warn!(error = %err, "reconcile failed; requeueing");
    Action::requeue(Duration::from_secs(10))
}

async fn reconcile(fi: Arc<FrontendIntegration>, ctx: Arc<ContextData>) -> Result<Action, Error> {
    let fi_name = fi.name_any();
    let ns = fi.namespace().ok_or_else(|| Error::MissingNamespace {
        name: fi_name.clone(),
    })?;
    let client = ctx.client.clone();

    let fi_api = Api::<FrontendIntegration>::namespaced(client.clone(), &ns);
    let job_api = Api::<Job>::namespaced(client.clone(), &ns);
    let secret_api = Api::<Secret>::namespaced(client.clone(), &ns);
    let bundle_api = Api::<JSBundle>::namespaced(client.clone(), &ns);

    if fi.meta().deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }

    if fi.spec.paused() {
        patch_fi_status(
            &fi_api,
            &fi,
            FrontendIntegrationStatus {
                phase: Some(FrontendIntegrationPhase::Pending),
                observed_manifest_hash: fi
                    .status
                    .as_ref()
                    .and_then(|s| s.observed_manifest_hash.clone()),
                observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
                observed_force_rebuild_token: fi.spec.force_rebuild_token.clone(),
                active_build: fi.status.as_ref().and_then(|s| s.active_build.clone()),
                bundle_ref: fi.status.as_ref().and_then(|s| s.bundle_ref.clone()),
                message: Some("Paused".to_string()),
                conditions: vec![],
            },
        )
        .await?;
        return Ok(Action::await_change());
    }

    let (manifest_content, manifest_hash) =
        manifest_content_and_hash(&fi.spec.source).context(CommonSnafu)?;
    if manifest_content.len() > MAX_SECRET_PAYLOAD_BYTES {
        let status = failed_status(
            &fi,
            &manifest_hash,
            format!(
                "manifest payload too large for Secret: {} bytes",
                manifest_content.len()
            ),
        );
        patch_fi_status(&fi_api, &fi, status).await?;
        return Err(Error::ManifestTooLarge {
            bytes: manifest_content.len(),
        });
    }

    let desired_bundle_name = fi
        .spec
        .bundle_name
        .clone()
        .unwrap_or_else(|| default_bundle_name(&fi_name));

    let needs_build = needs_new_build(&fi, &manifest_hash);
    if needs_build {
        let running_or_pending = find_job_for_hash(&job_api, &ns, &fi_name, &manifest_hash).await?;
        let chosen_job = if let Some(job) = running_or_pending {
            job
        } else {
            let nonce = time_nonce();
            let job_name = job_name(&fi_name, &manifest_hash, &nonce);
            let secret_name = secret_name(&fi_name, &manifest_hash, &nonce);
            let desired_job = make_build_job(
                &fi,
                &ctx.config,
                &job_name,
                &secret_name,
                &desired_bundle_name,
                &manifest_hash,
            );
            let created_job = create_or_get_job(&job_api, &ns, desired_job, &job_name).await?;
            let desired_secret = make_manifest_secret(
                &fi,
                &created_job,
                &secret_name,
                &manifest_hash,
                &manifest_content,
            );
            create_or_get_secret(&secret_api, &ns, desired_secret, &secret_name).await?;
            created_job
        };

        let status = building_status(
            &fi,
            &manifest_hash,
            &desired_bundle_name,
            &chosen_job,
            "Build job scheduled",
        );
        patch_fi_status(&fi_api, &fi, status).await?;
        return Ok(Action::requeue(Duration::from_secs(
            ctx.config.reconcile_requeue_seconds,
        )));
    }

    let action = sync_status_from_children(
        &fi,
        &fi_api,
        &job_api,
        &bundle_api,
        &ns,
        &desired_bundle_name,
        &manifest_hash,
        ctx.config.reconcile_requeue_seconds,
    )
    .await?;

    Ok(action)
}

fn needs_new_build(fi: &FrontendIntegration, manifest_hash: &str) -> bool {
    let status = fi.status.as_ref();
    let observed_hash = status.and_then(|s| s.observed_manifest_hash.as_deref());
    let observed_force = status.and_then(|s| s.observed_force_rebuild_token.as_deref());
    let phase = status.and_then(|s| s.phase.clone());

    let hash_changed = observed_hash != Some(manifest_hash);
    let force_changed = observed_force != fi.spec.force_rebuild_token.as_deref();
    let pending_initial = phase.is_none();

    hash_changed || force_changed || pending_initial
}

async fn sync_status_from_children(
    fi: &FrontendIntegration,
    fi_api: &Api<FrontendIntegration>,
    job_api: &Api<Job>,
    bundle_api: &Api<JSBundle>,
    namespace: &str,
    bundle_name: &str,
    manifest_hash: &str,
    requeue_seconds: u64,
) -> Result<Action, Error> {
    let fi_name = fi.name_any();
    let current_job = find_job_for_hash(job_api, namespace, &fi_name, manifest_hash).await?;

    if let Some(job) = current_job {
        match observed_job_phase(job.status.as_ref()) {
            ObservedJobPhase::Pending | ObservedJobPhase::Running => {
                let status =
                    building_status(fi, manifest_hash, bundle_name, &job, "Build in progress");
                patch_fi_status(fi_api, fi, status).await?;
                return Ok(Action::requeue(Duration::from_secs(requeue_seconds)));
            }
            ObservedJobPhase::Failed => {
                let msg =
                    extract_job_message(&job).unwrap_or_else(|| "Build job failed".to_string());
                let status = failed_status(fi, manifest_hash, msg);
                patch_fi_status(fi_api, fi, status).await?;
                return Ok(Action::await_change());
            }
            ObservedJobPhase::Succeeded => {
                let bundle = get_bundle_opt(bundle_api, namespace, bundle_name).await?;
                if let Some(bundle) = bundle {
                    if bundle.spec.manifest_hash == manifest_hash {
                        let status = succeeded_status(fi, manifest_hash, &bundle, &job);
                        patch_fi_status(fi_api, fi, status).await?;
                        return Ok(Action::await_change());
                    }
                    let status = failed_status(
                        fi,
                        manifest_hash,
                        format!(
                            "Job succeeded but JSBundle {} manifestHash mismatch (expected {}, got {})",
                            bundle_name, manifest_hash, bundle.spec.manifest_hash
                        ),
                    );
                    patch_fi_status(fi_api, fi, status).await?;
                    return Ok(Action::await_change());
                }

                let status = failed_status(
                    fi,
                    manifest_hash,
                    format!("Job succeeded but JSBundle {} was not found", bundle_name),
                );
                patch_fi_status(fi_api, fi, status).await?;
                return Ok(Action::await_change());
            }
        }
    }

    if let Some(bundle) = get_bundle_opt(bundle_api, namespace, bundle_name).await? {
        if bundle.spec.manifest_hash == manifest_hash {
            let status = FrontendIntegrationStatus {
                phase: Some(FrontendIntegrationPhase::Succeeded),
                observed_manifest_hash: Some(manifest_hash.to_string()),
                observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
                observed_force_rebuild_token: fi.spec.force_rebuild_token.clone(),
                active_build: fi.status.as_ref().and_then(|s| s.active_build.clone()),
                bundle_ref: Some(resource_ref(&bundle)),
                message: Some("JSBundle ready".to_string()),
                conditions: vec![],
            };
            patch_fi_status(fi_api, fi, status).await?;
        }
    }

    Ok(Action::await_change())
}

async fn find_job_for_hash(
    job_api: &Api<Job>,
    namespace: &str,
    fi_name: &str,
    manifest_hash: &str,
) -> Result<Option<Job>, Error> {
    let selector = format!(
        "{}={},{}={}",
        LABEL_FI_NAME,
        fi_name,
        LABEL_MANIFEST_HASH,
        manifest_hash_label_value(manifest_hash)
    );
    let jobs = job_api
        .list(&ListParams::default().labels(&selector))
        .await
        .with_context(|_| ListJobsForHashSnafu {
            namespace: namespace.to_string(),
            fi_name: fi_name.to_string(),
            manifest_hash: manifest_hash.to_string(),
        })?;
    let mut items = jobs.items;
    items.sort_by_key(|j| j.metadata.creation_timestamp.clone());
    Ok(items.pop())
}

fn observed_job_phase(status: Option<&JobStatus>) -> ObservedJobPhase {
    let Some(status) = status else {
        return ObservedJobPhase::Pending;
    };

    if status.failed.unwrap_or(0) > 0 {
        return ObservedJobPhase::Failed;
    }
    if status.succeeded.unwrap_or(0) > 0 {
        return ObservedJobPhase::Succeeded;
    }
    if status.active.unwrap_or(0) > 0 {
        return ObservedJobPhase::Running;
    }

    if let Some(conditions) = &status.conditions {
        for cond in conditions {
            if cond.status != "True" {
                continue;
            }
            if cond.type_ == "Failed" {
                return ObservedJobPhase::Failed;
            }
            if cond.type_ == "Complete" {
                return ObservedJobPhase::Succeeded;
            }
        }
    }

    ObservedJobPhase::Pending
}

fn extract_job_message(job: &Job) -> Option<String> {
    let status = job.status.as_ref()?;
    if let Some(conditions) = &status.conditions {
        if let Some(cond) = conditions
            .iter()
            .find(|c| c.status == "True" && c.type_ == "Failed")
        {
            return cond.message.clone().or_else(|| cond.reason.clone());
        }
    }
    None
}

fn manifest_hash_label_value(hash: &str) -> String {
    hash.strip_prefix("sha256:").unwrap_or(hash).to_string()
}

fn labels_for(fi_name: &str, manifest_hash: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_MANAGED_BY.to_string(), MANAGED_BY_VALUE.to_string()),
        (LABEL_FI_NAME.to_string(), fi_name.to_string()),
        (
            LABEL_MANIFEST_HASH.to_string(),
            manifest_hash_label_value(manifest_hash),
        ),
    ])
}

fn base_owner_ref<T>(obj: &T) -> Option<OwnerReference>
where
    T: Resource<DynamicType = ()>,
{
    obj.controller_owner_ref(&())
}

fn make_build_job(
    fi: &FrontendIntegration,
    config: &ControllerConfig,
    job_name: &str,
    secret_name: &str,
    jsbundle_name: &str,
    manifest_hash: &str,
) -> Job {
    let fi_name = fi.name_any();
    let ns = fi.namespace();
    let mut labels = labels_for(&fi_name, manifest_hash);
    labels.insert(LABEL_BUILD_KIND.to_string(), BUILD_KIND_VALUE.to_string());

    let mut annotations = BTreeMap::new();
    if let Some(generation) = fi.metadata.generation {
        annotations.insert(ANNO_OBSERVED_GENERATION.to_string(), generation.to_string());
    }

    let env = vec![
        EnvVar {
            name: "FI_NAMESPACE".to_string(),
            value: ns,
            ..Default::default()
        },
        EnvVar {
            name: "FI_NAME".to_string(),
            value: Some(fi_name.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "MANIFEST_HASH".to_string(),
            value: Some(manifest_hash.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "MANIFEST_PATH".to_string(),
            value: Some(DEFAULT_MANIFEST_MOUNT_PATH.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "JSBUNDLE_NAME".to_string(),
            value: Some(jsbundle_name.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "BUILD_SERVICE_BASE_URL".to_string(),
            value: Some(config.build_service_base_url.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "BUILD_SERVICE_TIMEOUT_SECONDS".to_string(),
            value: Some(config.build_service_timeout_seconds.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "STALE_CHECK_GRACE_SECONDS".to_string(),
            value: Some(config.stale_check_grace_seconds.to_string()),
            ..Default::default()
        },
    ];

    let container = Container {
        name: "runner".to_string(),
        image: Some(config.runner_image.clone()),
        env: Some(env),
        volume_mounts: Some(vec![VolumeMount {
            name: "manifest".to_string(),
            mount_path: "/work/manifest".to_string(),
            read_only: Some(true),
            ..Default::default()
        }]),
        ..Default::default()
    };

    Job {
        metadata: ObjectMeta {
            name: Some(job_name.to_string()),
            namespace: fi.namespace(),
            labels: Some(labels),
            annotations: Some(annotations),
            owner_references: base_owner_ref(fi).map(|o| vec![o]),
            ..Default::default()
        },
        spec: Some(JobSpec {
            ttl_seconds_after_finished: config.job_ttl_seconds_after_finished,
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(BTreeMap::from([(
                        "app.kubernetes.io/name".to_string(),
                        "frontend-forge-runner".to_string(),
                    )])),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_string()),
                    service_account_name: config.runner_service_account.clone(),
                    containers: vec![container],
                    volumes: Some(vec![Volume {
                        name: "manifest".to_string(),
                        secret: Some(SecretVolumeSource {
                            secret_name: Some(secret_name.to_string()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
            },
            backoff_limit: Some(0),
            ..Default::default()
        }),
        status: None,
    }
}

fn make_manifest_secret(
    fi: &FrontendIntegration,
    job: &Job,
    secret_name: &str,
    manifest_hash: &str,
    manifest_content: &str,
) -> Secret {
    let fi_name = fi.name_any();
    let mut labels = labels_for(&fi_name, manifest_hash);
    labels.insert(LABEL_BUILD_KIND.to_string(), BUILD_KIND_VALUE.to_string());

    Secret {
        metadata: ObjectMeta {
            name: Some(secret_name.to_string()),
            namespace: fi.namespace(),
            labels: Some(labels),
            owner_references: base_owner_ref(job).map(|o| vec![o]),
            ..Default::default()
        },
        immutable: Some(true),
        string_data: Some(BTreeMap::from([(
            DEFAULT_MANIFEST_FILENAME.to_string(),
            manifest_content.to_string(),
        )])),
        type_: Some("Opaque".to_string()),
        ..Default::default()
    }
}

async fn create_or_get_job(
    job_api: &Api<Job>,
    namespace: &str,
    job: Job,
    name: &str,
) -> Result<Job, Error> {
    match job_api.create(&PostParams::default(), &job).await {
        Ok(created) => Ok(created),
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            Ok(job_api
                .get(name)
                .await
                .with_context(|_| GetJobAfterConflictSnafu {
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                })?)
        }
        Err(err) => Err(Error::CreateJob {
            namespace: namespace.to_string(),
            name: name.to_string(),
            source: err,
        }),
    }
}

async fn create_or_get_secret(
    secret_api: &Api<Secret>,
    namespace: &str,
    secret: Secret,
    name: &str,
) -> Result<Secret, Error> {
    match secret_api.create(&PostParams::default(), &secret).await {
        Ok(created) => Ok(created),
        Err(kube::Error::Api(ae)) if ae.code == 409 => Ok(secret_api
            .get(name)
            .await
            .with_context(|_| GetSecretAfterConflictSnafu {
                namespace: namespace.to_string(),
                name: name.to_string(),
            })?),
        Err(err) => Err(Error::CreateSecret {
            namespace: namespace.to_string(),
            name: name.to_string(),
            source: err,
        }),
    }
}

async fn get_bundle_opt(
    bundle_api: &Api<JSBundle>,
    namespace: &str,
    name: &str,
) -> Result<Option<JSBundle>, Error> {
    bundle_api
        .get_opt(name)
        .await
        .with_context(|_| GetJsBundleSnafu {
            namespace: namespace.to_string(),
            name: name.to_string(),
        })
}

fn resource_ref<K: ResourceExt>(obj: &K) -> ResourceRef {
    ResourceRef {
        name: obj.name_any(),
        namespace: obj.namespace(),
        uid: obj.meta().uid.clone(),
    }
}

fn building_status(
    fi: &FrontendIntegration,
    manifest_hash: &str,
    bundle_name: &str,
    job: &Job,
    message: &str,
) -> FrontendIntegrationStatus {
    FrontendIntegrationStatus {
        phase: Some(FrontendIntegrationPhase::Building),
        observed_manifest_hash: Some(manifest_hash.to_string()),
        observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
        observed_force_rebuild_token: fi.spec.force_rebuild_token.clone(),
        active_build: Some(ActiveBuildStatus {
            job_ref: Some(resource_ref(job)),
            started_at: Some(Utc::now()),
        }),
        bundle_ref: Some(ResourceRef {
            name: bundle_name.to_string(),
            namespace: fi.namespace(),
            uid: None,
        }),
        message: Some(message.to_string()),
        conditions: vec![],
    }
}

fn succeeded_status(
    fi: &FrontendIntegration,
    manifest_hash: &str,
    bundle: &JSBundle,
    job: &Job,
) -> FrontendIntegrationStatus {
    FrontendIntegrationStatus {
        phase: Some(FrontendIntegrationPhase::Succeeded),
        observed_manifest_hash: Some(manifest_hash.to_string()),
        observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
        observed_force_rebuild_token: fi.spec.force_rebuild_token.clone(),
        active_build: Some(ActiveBuildStatus {
            job_ref: Some(resource_ref(job)),
            started_at: fi
                .status
                .as_ref()
                .and_then(|s| s.active_build.clone())
                .and_then(|b| b.started_at),
        }),
        bundle_ref: Some(resource_ref(bundle)),
        message: Some("Build succeeded".to_string()),
        conditions: vec![],
    }
}

fn failed_status(
    fi: &FrontendIntegration,
    manifest_hash: &str,
    message: String,
) -> FrontendIntegrationStatus {
    FrontendIntegrationStatus {
        phase: Some(FrontendIntegrationPhase::Failed),
        observed_manifest_hash: Some(manifest_hash.to_string()),
        observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
        observed_force_rebuild_token: fi.spec.force_rebuild_token.clone(),
        active_build: fi.status.as_ref().and_then(|s| s.active_build.clone()),
        bundle_ref: fi.status.as_ref().and_then(|s| s.bundle_ref.clone()),
        message: Some(message),
        conditions: vec![],
    }
}

async fn patch_fi_status(
    fi_api: &Api<FrontendIntegration>,
    fi: &FrontendIntegration,
    status: FrontendIntegrationStatus,
) -> Result<(), Error> {
    let fi_name = fi.name_any();
    let namespace = fi.namespace().unwrap_or_else(|| "<cluster>".to_string());
    let patch = json!({
        "status": status,
    });

    fi_api
        .patch_status(&fi_name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .with_context(|_| PatchFrontendIntegrationStatusSnafu {
            namespace,
            name: fi_name.clone(),
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use frontend_forge_api::FrontendIntegrationSpec;
    use kube::core::ObjectMeta;
    use serde_json::json;

    fn fi(name: &str, status: Option<FrontendIntegrationStatus>) -> FrontendIntegration {
        FrontendIntegration {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                generation: Some(3),
                ..Default::default()
            },
            spec: FrontendIntegrationSpec {
                source: json!({"foo": "bar"}),
                bundle_name: None,
                force_rebuild_token: None,
                paused: None,
            },
            status,
        }
    }

    #[test]
    fn needs_build_when_hash_changes() {
        let fi = fi(
            "demo",
            Some(FrontendIntegrationStatus {
                observed_manifest_hash: Some("sha256:old".to_string()),
                phase: Some(FrontendIntegrationPhase::Succeeded),
                ..Default::default()
            }),
        );

        assert!(needs_new_build(&fi, "sha256:new"));
    }

    #[test]
    fn hash_label_value_is_dns_safe() {
        assert_eq!(manifest_hash_label_value("sha256:abcd"), "abcd");
        assert_eq!(manifest_hash_label_value("abcd"), "abcd");
    }
}
