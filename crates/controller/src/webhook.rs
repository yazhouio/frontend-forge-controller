use axum::{
    Json, Router,
    body::Bytes,
    routing::{get, post},
};
use axum_server::{Handle, tls_rustls::RustlsConfig};
use frontend_forge_api::FrontendIntegration;
use frontend_forge_manifest::validate_frontend_integration;
use kube::Resource;
use kube::core::{
    DynamicObject, Status,
    admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, Operation},
};
use snafu::ResultExt;
use std::{env, future::pending, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};
use tracing::info;

use crate::{
    Error, InvalidWebhookBindAddrSnafu, InvalidWebhookEnabledSnafu, WebhookServerSnafu,
    WebhookTlsConfigSnafu,
};

const DEFAULT_WEBHOOK_BIND_ADDR: &str = "0.0.0.0:9443";
const DEFAULT_WEBHOOK_CERT_PATH: &str = "/tls/tls.crt";
const DEFAULT_WEBHOOK_KEY_PATH: &str = "/tls/tls.key";
const WEBHOOK_SHUTDOWN_GRACE_PERIOD_SECONDS: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebhookConfig {
    pub(crate) enabled: bool,
    pub(crate) bind_addr: SocketAddr,
    pub(crate) cert_path: PathBuf,
    pub(crate) key_path: PathBuf,
}

impl WebhookConfig {
    pub(crate) fn from_env() -> Result<Self, Error> {
        let enabled = env::var("WEBHOOK_ENABLED")
            .ok()
            .map(|value| {
                value
                    .parse::<bool>()
                    .with_context(|_| InvalidWebhookEnabledSnafu {
                        value: value.clone(),
                    })
            })
            .transpose()?
            .unwrap_or(false);
        let bind_addr_raw =
            env::var("WEBHOOK_BIND_ADDR").unwrap_or_else(|_| DEFAULT_WEBHOOK_BIND_ADDR.to_string());
        let bind_addr =
            SocketAddr::from_str(&bind_addr_raw).with_context(|_| InvalidWebhookBindAddrSnafu {
                value: bind_addr_raw.clone(),
            })?;

        Ok(Self {
            enabled,
            bind_addr,
            cert_path: PathBuf::from(
                env::var("WEBHOOK_CERT_PATH")
                    .unwrap_or_else(|_| DEFAULT_WEBHOOK_CERT_PATH.to_string()),
            ),
            key_path: PathBuf::from(
                env::var("WEBHOOK_KEY_PATH")
                    .unwrap_or_else(|_| DEFAULT_WEBHOOK_KEY_PATH.to_string()),
            ),
        })
    }
}

pub(crate) async fn run_webhook_server(config: WebhookConfig) -> Result<(), Error> {
    let tls_config = load_tls_config(&config)
        .await?
        .expect("enabled webhook must load TLS config");
    let app = router();
    let handle = Handle::new();

    tokio::spawn(shutdown_webhook_on_signal(handle.clone()));

    info!(bind_addr = %config.bind_addr, "webhook server listening");
    axum_server::bind_rustls(config.bind_addr, tls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await
        .with_context(|_| WebhookServerSnafu {
            bind_addr: config.bind_addr,
        })
}

async fn shutdown_webhook_on_signal(handle: Handle<SocketAddr>) {
    shutdown_signal().await;
    handle.graceful_shutdown(Some(Duration::from_secs(
        WEBHOOK_SHUTDOWN_GRACE_PERIOD_SECONDS,
    )));
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => {
                pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn load_tls_config(config: &WebhookConfig) -> Result<Option<RustlsConfig>, Error> {
    if !config.enabled {
        return Ok(None);
    }

    Ok(Some(
        RustlsConfig::from_pem_file(&config.cert_path, &config.key_path)
            .await
            .with_context(|_| WebhookTlsConfigSnafu {
                cert_path: config.cert_path.display().to_string(),
                key_path: config.key_path.display().to_string(),
            })?,
    ))
}

fn router() -> Router {
    Router::new().route("/healthz", get(healthz)).route(
        "/validate/frontendintegrations",
        post(validate_frontend_integrations),
    )
}

async fn healthz() -> &'static str {
    "ok"
}

async fn validate_frontend_integrations(body: Bytes) -> Json<AdmissionReview<DynamicObject>> {
    Json(process_validation_request(body.as_ref()))
}

fn process_validation_request(body: &[u8]) -> AdmissionReview<DynamicObject> {
    match serde_json::from_slice::<AdmissionReview<FrontendIntegration>>(body) {
        Ok(review) => validate_review(review),
        Err(err) => {
            AdmissionResponse::invalid(format!("failed to deserialize AdmissionReview: {err}"))
                .into_review()
        }
    }
}

fn validate_review(review: AdmissionReview<FrontendIntegration>) -> AdmissionReview<DynamicObject> {
    let request: AdmissionRequest<FrontendIntegration> = match review.try_into() {
        Ok(request) => request,
        Err(_) => {
            return AdmissionResponse::invalid("admission review.request is required")
                .into_review();
        }
    };

    let response = match request.operation {
        Operation::Create | Operation::Update => validate_request_object(&request),
        _ => AdmissionResponse::from(&request),
    };

    response.into_review()
}

fn validate_request_object(request: &AdmissionRequest<FrontendIntegration>) -> AdmissionResponse {
    let Some(fi) = request.object.as_ref() else {
        return invalid_response(
            request,
            format!(
                "admission request.object is required for {:?}",
                request.operation
            ),
        );
    };

    match validate_frontend_integration(fi) {
        Ok(()) => AdmissionResponse::from(request),
        Err(err) => AdmissionResponse::from(request).deny(err.to_string()),
    }
}

fn invalid_response<T: Resource>(
    request: &AdmissionRequest<T>,
    message: impl Into<String>,
) -> AdmissionResponse {
    let message = message.into();
    let mut response = AdmissionResponse::from(request);
    response.allowed = false;
    response.result = Status::failure(&message, "InvalidRequest");
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use frontend_forge_api::{
        FrontendIntegrationSpec, IframePageSpec, MenuNodeType, MenuPlacement, PageSpec, PageType,
        PrimaryMenuSpec,
    };
    use kube::core::ObjectMeta;
    use serde_json::json;

    fn frontend_integration(name: &str) -> FrontendIntegration {
        FrontendIntegration {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: FrontendIntegrationSpec {
                display_name: None,
                locales: Default::default(),
                enabled: Some(true),
                menus: vec![PrimaryMenuSpec {
                    display_name: "demo".to_string(),
                    key: "demo".to_string(),
                    icon: None,
                    placement: MenuPlacement::Global,
                    type_: MenuNodeType::Page,
                    children: vec![],
                }],
                pages: vec![PageSpec {
                    key: "demo".to_string(),
                    type_: PageType::Iframe,
                    crd_table: None,
                    iframe: Some(IframePageSpec {
                        src: "http://example.test".to_string(),
                    }),
                }],
                builder: None,
            },
            status: None,
        }
    }

    fn review_bytes(operation: &str, object: Option<serde_json::Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "apiVersion": "admission.k8s.io/v1",
            "kind": "AdmissionReview",
            "request": {
                "uid": "req-1",
                "kind": {
                    "group": "frontend-forge.kubesphere.io",
                    "version": "v1alpha1",
                    "kind": "FrontendIntegration"
                },
                "resource": {
                    "group": "frontend-forge.kubesphere.io",
                    "version": "v1alpha1",
                    "resource": "frontendintegrations"
                },
                "name": "demo",
                "operation": operation,
                "userInfo": {},
                "object": object,
                "oldObject": null,
                "dryRun": false,
                "options": null
            }
        }))
        .unwrap()
    }

    fn response_for(body: &[u8]) -> AdmissionResponse {
        process_validation_request(body)
            .response
            .expect("admission response")
    }

    #[test]
    fn create_request_allows_valid_object() {
        let fi = frontend_integration("demo");
        let response = response_for(&review_bytes(
            "CREATE",
            Some(serde_json::to_value(fi).unwrap()),
        ));

        assert!(response.allowed);
        assert_eq!(response.uid, "req-1");
    }

    #[test]
    fn update_request_denies_duplicate_page_key() {
        let mut fi = frontend_integration("demo");
        fi.spec.pages.push(PageSpec {
            key: "demo".to_string(),
            type_: PageType::Iframe,
            crd_table: None,
            iframe: Some(IframePageSpec {
                src: "http://example.test/other".to_string(),
            }),
        });

        let response = response_for(&review_bytes(
            "UPDATE",
            Some(serde_json::to_value(fi).unwrap()),
        ));

        assert!(!response.allowed);
        assert_eq!(
            response.result.message,
            "FrontendIntegration demo has duplicate page key 'demo'"
        );
    }

    #[test]
    fn create_request_without_object_is_invalid() {
        let response = response_for(&review_bytes("CREATE", None));

        assert!(!response.allowed);
        assert_eq!(response.result.reason, "InvalidRequest");
        assert_eq!(
            response.result.message,
            "admission request.object is required for Create"
        );
    }

    #[test]
    fn malformed_body_returns_invalid_review() {
        let response = response_for(br#"{"request": "bad""#);

        assert!(!response.allowed);
        assert_eq!(response.result.reason, "InvalidRequest");
        assert!(
            response
                .result
                .message
                .contains("failed to deserialize AdmissionReview")
        );
    }

    #[test]
    fn delete_request_is_allowed_without_object() {
        let response = response_for(&review_bytes("DELETE", None));

        assert!(response.allowed);
        assert_eq!(response.uid, "req-1");
    }

    #[tokio::test]
    async fn disabled_webhook_does_not_read_tls_files() {
        let config = WebhookConfig {
            enabled: false,
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 9443)),
            cert_path: PathBuf::from("/tmp/does-not-exist-cert.pem"),
            key_path: PathBuf::from("/tmp/does-not-exist-key.pem"),
        };

        let tls = load_tls_config(&config).await.unwrap();
        assert!(tls.is_none());
    }

    #[tokio::test]
    async fn enabled_webhook_requires_tls_files() {
        let config = WebhookConfig {
            enabled: true,
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 9443)),
            cert_path: PathBuf::from("/tmp/does-not-exist-cert.pem"),
            key_path: PathBuf::from("/tmp/does-not-exist-key.pem"),
        };

        assert!(matches!(
            load_tls_config(&config).await,
            Err(Error::WebhookTlsConfig { .. })
        ));
    }
}
