use chrono::Utc;
use frontend_forge_api::{
    ActiveBuildStatus, FrontendIntegration, FrontendIntegrationPhase, FrontendIntegrationStatus,
    JSBundle, ResourceRef,
};
use frontend_forge_common::{
    ANNO_MANIFEST_HASH, ANNO_OBSERVED_GENERATION, BUILD_KIND_VALUE, CommonError, LABEL_BUILD_KIND,
    LABEL_FI_NAME, LABEL_MANAGED_BY, LABEL_MANIFEST_HASH, LABEL_SPEC_HASH, MANAGED_BY_VALUE,
    default_bundle_name, job_name, serializable_hash, time_nonce,
};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::JobStatus;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{Container, EnvVar, PodSpec, PodTemplateSpec};
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
    #[snafu(display("spec/hash error: {source}"))]
    Common { source: CommonError },
    #[snafu(display("failed to initialize Kubernetes client: {source}"))]
    KubeClientInit { source: kube::Error },
    #[snafu(display("failed to patch FrontendIntegration status {namespace}/{name}: {source}"))]
    PatchFrontendIntegrationStatus {
        namespace: String,
        name: String,
        source: kube::Error,
    },
    #[snafu(display(
        "failed to list Jobs in {namespace} for FrontendIntegration {fi_name} and specHash {spec_hash}: {source}"
    ))]
    ListJobsForHash {
        namespace: String,
        fi_name: String,
        spec_hash: String,
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
}

#[derive(Clone, Debug)]
struct ControllerConfig {
    work_namespace: String,
    runner_image: String,
    runner_service_account: Option<String>,
    build_service_base_url: String,
    jsbundle_configmap_namespace: String,
    jsbundle_config_key: String,
    build_service_timeout_seconds: u64,
    stale_check_grace_seconds: u64,
    reconcile_requeue_seconds: u64,
    job_ttl_seconds_after_finished: Option<i32>,
}

impl ControllerConfig {
    fn from_env() -> Self {
        Self {
            work_namespace: env::var("WORK_NAMESPACE")
                .unwrap_or_else(|_| "extension-frontend-forge".to_string()),
            runner_image: env::var("RUNNER_IMAGE")
                .unwrap_or_else(|_| "ghcr.io/example/frontend-forge-runner:latest".to_string()),
            runner_service_account: env::var("RUNNER_SERVICE_ACCOUNT").ok(),
            build_service_base_url: env::var("BUILD_SERVICE_BASE_URL").unwrap_or_else(|_| {
                "http://build-service.extension-frontend-forge.svc.cluster.local".to_string()
            }),
            jsbundle_configmap_namespace: env::var("JSBUNDLE_CONFIGMAP_NAMESPACE")
                .unwrap_or_else(|_| "extension-frontend-forge".to_string()),
            jsbundle_config_key: env::var("JSBUNDLE_CONFIG_KEY")
                .unwrap_or_else(|_| "index.js".to_string()),
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
    Controller::new(fi_api, watcher::Config::default())
        .owns(job_api, watcher::Config::default())
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
    let client = ctx.client.clone();
    let work_ns = ctx.config.work_namespace.clone();

    let fi_api = Api::<FrontendIntegration>::all(client.clone());
    let job_api = Api::<Job>::namespaced(client.clone(), &work_ns);
    let bundle_api = Api::<JSBundle>::all(client.clone());

    if fi.meta().deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }

    if !fi.spec.enabled() {
        patch_fi_status(
            &fi_api,
            &fi,
            FrontendIntegrationStatus {
                phase: Some(FrontendIntegrationPhase::Pending),
                observed_spec_hash: fi
                    .status
                    .as_ref()
                    .and_then(|s| s.observed_spec_hash.clone()),
                observed_manifest_hash: fi
                    .status
                    .as_ref()
                    .and_then(|s| s.observed_manifest_hash.clone()),
                observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
                active_build: fi.status.as_ref().and_then(|s| s.active_build.clone()),
                bundle_ref: fi.status.as_ref().and_then(|s| s.bundle_ref.clone()),
                message: Some("Disabled".to_string()),
                conditions: vec![],
            },
        )
        .await?;
        return Ok(Action::await_change());
    }

    let spec_hash = serializable_hash(&fi.spec).context(CommonSnafu)?;

    let desired_bundle_name = default_bundle_name(&fi_name);

    let needs_build = needs_new_build(&fi, &spec_hash);
    if needs_build {
        let running_or_pending =
            find_job_for_hash(&job_api, &work_ns, &fi_name, &spec_hash).await?;
        let chosen_job = if let Some(job) = running_or_pending {
            job
        } else {
            let nonce = time_nonce();
            let job_name = job_name(&fi_name, &spec_hash, &nonce);
            let desired_job = make_build_job(
                &fi,
                &ctx.config,
                &job_name,
                &desired_bundle_name,
                &spec_hash,
            );
            let created_job = create_or_get_job(&job_api, &work_ns, desired_job, &job_name).await?;
            created_job
        };

        let status = building_status(
            &fi,
            &spec_hash,
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
        &work_ns,
        &desired_bundle_name,
        &spec_hash,
        ctx.config.reconcile_requeue_seconds,
    )
    .await?;

    Ok(action)
}

fn needs_new_build(fi: &FrontendIntegration, spec_hash: &str) -> bool {
    let status = fi.status.as_ref();
    let observed_hash = status
        .and_then(|s| s.observed_spec_hash.as_deref())
        .or_else(|| status.and_then(|s| s.observed_manifest_hash.as_deref()));
    let phase = status.and_then(|s| s.phase.clone());

    let hash_changed = observed_hash != Some(spec_hash);
    let pending_initial = phase.is_none();
    let retry_failed = matches!(phase, Some(FrontendIntegrationPhase::Failed));

    hash_changed || pending_initial || retry_failed
}

async fn sync_status_from_children(
    fi: &FrontendIntegration,
    fi_api: &Api<FrontendIntegration>,
    job_api: &Api<Job>,
    bundle_api: &Api<JSBundle>,
    namespace: &str,
    bundle_name: &str,
    spec_hash: &str,
    requeue_seconds: u64,
) -> Result<Action, Error> {
    let fi_name = fi.name_any();
    let current_job = find_job_for_hash(job_api, namespace, &fi_name, spec_hash).await?;

    if let Some(job) = current_job {
        match observed_job_phase(job.status.as_ref()) {
            ObservedJobPhase::Pending | ObservedJobPhase::Running => {
                let status = building_status(fi, spec_hash, bundle_name, &job, "Build in progress");
                patch_fi_status(fi_api, fi, status).await?;
                return Ok(Action::requeue(Duration::from_secs(requeue_seconds)));
            }
            ObservedJobPhase::Failed => {
                let msg =
                    extract_job_message(&job).unwrap_or_else(|| "Build job failed".to_string());
                let status = failed_status(fi, spec_hash, msg);
                patch_fi_status(fi_api, fi, status).await?;
                return Ok(Action::await_change());
            }
            ObservedJobPhase::Succeeded => {
                let bundle = get_bundle_opt(bundle_api, bundle_name).await?;
                if let Some(bundle) = bundle {
                    if bundle_matches_spec_hash(&bundle, spec_hash) {
                        let status = succeeded_status(fi, spec_hash, &bundle, &job);
                        patch_fi_status(fi_api, fi, status).await?;
                        return Ok(Action::await_change());
                    }
                    let status = building_status(
                        fi,
                        spec_hash,
                        bundle_name,
                        &job,
                        "Job succeeded; waiting for JSBundle with matching spec-hash",
                    );
                    patch_fi_status(fi_api, fi, status).await?;
                    return Ok(Action::requeue(Duration::from_secs(requeue_seconds)));
                }

                let status = building_status(
                    fi,
                    spec_hash,
                    bundle_name,
                    &job,
                    "Job succeeded; waiting for JSBundle materialization",
                );
                patch_fi_status(fi_api, fi, status).await?;
                return Ok(Action::requeue(Duration::from_secs(requeue_seconds)));
            }
        }
    }

    if let Some(bundle) = get_bundle_opt(bundle_api, bundle_name).await? {
        if bundle_matches_spec_hash(&bundle, spec_hash) {
            let status = FrontendIntegrationStatus {
                phase: Some(FrontendIntegrationPhase::Succeeded),
                observed_spec_hash: Some(spec_hash.to_string()),
                observed_manifest_hash: bundle_manifest_hash(&bundle),
                observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
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
    spec_hash: &str,
) -> Result<Option<Job>, Error> {
    let selector = format!(
        "{}={},{}={}",
        LABEL_FI_NAME,
        fi_name,
        LABEL_SPEC_HASH,
        hash_label_value(spec_hash)
    );
    let jobs = job_api
        .list(&ListParams::default().labels(&selector))
        .await
        .with_context(|_| ListJobsForHashSnafu {
            namespace: namespace.to_string(),
            fi_name: fi_name.to_string(),
            spec_hash: spec_hash.to_string(),
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

fn hash_label_value(hash: &str) -> String {
    hash.strip_prefix("sha256:").unwrap_or(hash).to_string()
}

fn bundle_matches_spec_hash(bundle: &JSBundle, spec_hash: &str) -> bool {
    let expected = hash_label_value(spec_hash);
    bundle
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(LABEL_SPEC_HASH))
        .map(|v| v == &expected)
        .unwrap_or(false)
}

fn labels_for(fi_name: &str, spec_hash: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_MANAGED_BY.to_string(), MANAGED_BY_VALUE.to_string()),
        (LABEL_FI_NAME.to_string(), fi_name.to_string()),
        (LABEL_SPEC_HASH.to_string(), hash_label_value(spec_hash)),
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
    jsbundle_name: &str,
    spec_hash: &str,
) -> Job {
    let fi_name = fi.name_any();
    let mut labels = labels_for(&fi_name, spec_hash);
    labels.insert(LABEL_BUILD_KIND.to_string(), BUILD_KIND_VALUE.to_string());

    let mut annotations = BTreeMap::new();
    if let Some(generation) = fi.metadata.generation {
        annotations.insert(ANNO_OBSERVED_GENERATION.to_string(), generation.to_string());
    }

    let env = vec![
        EnvVar {
            name: "FI_NAME".to_string(),
            value: Some(fi_name.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "SPEC_HASH".to_string(),
            value: Some(spec_hash.to_string()),
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
            name: "JSBUNDLE_CONFIGMAP_NAMESPACE".to_string(),
            value: Some(config.jsbundle_configmap_namespace.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "JSBUNDLE_CONFIG_KEY".to_string(),
            value: Some(config.jsbundle_config_key.clone()),
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
        ..Default::default()
    };

    Job {
        metadata: ObjectMeta {
            name: Some(job_name.to_string()),
            namespace: Some(config.work_namespace.clone()),
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
                    ..Default::default()
                }),
            },
            backoff_limit: Some(0),
            ..Default::default()
        }),
        status: None,
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

async fn get_bundle_opt(bundle_api: &Api<JSBundle>, name: &str) -> Result<Option<JSBundle>, Error> {
    bundle_api
        .get_opt(name)
        .await
        .with_context(|_| GetJsBundleSnafu {
            namespace: "<cluster>".to_string(),
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
    spec_hash: &str,
    bundle_name: &str,
    job: &Job,
    message: &str,
) -> FrontendIntegrationStatus {
    FrontendIntegrationStatus {
        phase: Some(FrontendIntegrationPhase::Building),
        observed_spec_hash: Some(spec_hash.to_string()),
        observed_manifest_hash: fi
            .status
            .as_ref()
            .and_then(|s| s.observed_manifest_hash.clone()),
        observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
        active_build: Some(ActiveBuildStatus {
            job_ref: Some(resource_ref(job)),
            started_at: Some(Utc::now()),
        }),
        bundle_ref: Some(ResourceRef {
            name: bundle_name.to_string(),
            namespace: None,
            uid: None,
        }),
        message: Some(message.to_string()),
        conditions: vec![],
    }
}

fn succeeded_status(
    fi: &FrontendIntegration,
    spec_hash: &str,
    bundle: &JSBundle,
    job: &Job,
) -> FrontendIntegrationStatus {
    FrontendIntegrationStatus {
        phase: Some(FrontendIntegrationPhase::Succeeded),
        observed_spec_hash: Some(spec_hash.to_string()),
        observed_manifest_hash: bundle_manifest_hash(bundle),
        observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
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

fn bundle_manifest_hash(bundle: &JSBundle) -> Option<String> {
    if let Some(v) = bundle
        .metadata
        .annotations
        .as_ref()
        .and_then(|annos| annos.get(ANNO_MANIFEST_HASH))
        .cloned()
    {
        return Some(v);
    }

    bundle
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(LABEL_MANIFEST_HASH))
        .map(|v| {
            if v.starts_with("sha256:") {
                v.clone()
            } else {
                format!("sha256:{}", v)
            }
        })
}

fn failed_status(
    fi: &FrontendIntegration,
    spec_hash: &str,
    message: String,
) -> FrontendIntegrationStatus {
    FrontendIntegrationStatus {
        phase: Some(FrontendIntegrationPhase::Failed),
        observed_spec_hash: Some(spec_hash.to_string()),
        observed_manifest_hash: fi
            .status
            .as_ref()
            .and_then(|s| s.observed_manifest_hash.clone()),
        observed_generation: Some(fi.metadata.generation.unwrap_or_default()),
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
    use frontend_forge_api::{
        FrontendIntegrationSpec, IframeIntegrationSpec, IntegrationSpec, IntegrationType,
        RoutingSpec,
    };
    use kube::core::ObjectMeta;

    fn fi(name: &str, status: Option<FrontendIntegrationStatus>) -> FrontendIntegration {
        FrontendIntegration {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                generation: Some(3),
                ..Default::default()
            },
            spec: FrontendIntegrationSpec {
                display_name: None,
                enabled: Some(true),
                integration: IntegrationSpec {
                    type_: IntegrationType::Iframe,
                    crd: None,
                    iframe: Some(IframeIntegrationSpec {
                        src: "http://example.test".to_string(),
                    }),
                    menu: None,
                },
                routing: RoutingSpec {
                    path: "demo".to_string(),
                },
                columns: vec![],
                menu: None,
                builder: None,
            },
            status,
        }
    }

    #[test]
    fn needs_build_when_hash_changes() {
        let fi = fi(
            "demo",
            Some(FrontendIntegrationStatus {
                observed_spec_hash: Some("sha256:old".to_string()),
                phase: Some(FrontendIntegrationPhase::Succeeded),
                ..Default::default()
            }),
        );

        assert!(needs_new_build(&fi, "sha256:new"));
    }

    #[test]
    fn hash_label_value_is_dns_safe() {
        assert_eq!(hash_label_value("sha256:abcd"), "abcd");
        assert_eq!(hash_label_value("abcd"), "abcd");
    }
}
