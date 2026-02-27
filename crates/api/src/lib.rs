use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::Snafu;

pub const API_GROUP: &str = "frontend-forge.io";
pub const API_VERSION: &str = "v1alpha1";
pub const JSBUNDLE_PLURAL: &str = "jsbundles";
pub const JSBUNDLE_API_GROUP: &str = "extensions.kubesphere.io";
pub const JSBUNDLE_API_VERSION: &str = "v1alpha1";

#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[kube(
    group = "frontend-forge.io",
    version = "v1alpha1",
    kind = "FrontendIntegration",
    plural = "frontendintegrations",
    status = "FrontendIntegrationStatus",
    shortname = "fi"
)]
pub struct FrontendIntegrationSpec {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "displayName"
    )]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    pub integration: IntegrationSpec,
    pub routing: RoutingSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<MenuSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<BuilderSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BuilderSpec {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "engineVersion"
    )]
    pub engine_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct IntegrationSpec {
    #[serde(rename = "type")]
    pub type_: IntegrationType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crd: Option<CrdIntegrationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iframe: Option<IframeIntegrationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<IntegrationMenuSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IntegrationType {
    Crd,
    Iframe,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct IntegrationMenuSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct IframeIntegrationSpec {
    #[serde(alias = "url")]
    pub src: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CrdIntegrationSpec {
    pub names: CrdNamesSpec,
    pub group: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "authKey")]
    pub auth_key: Option<String>,
    pub scope: CrdScope,
    // Compatibility: Manifest.md example places columns under integration.crd.columns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CrdNamesSpec {
    pub kind: String,
    pub plural: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum CrdScope {
    Namespaced,
    Cluster,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RoutingSpec {
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ColumnSpec {
    pub key: String,
    pub title: String,
    pub render: ColumnRenderSpec,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "enableSorting"
    )]
    pub enable_sorting: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "enableHiding"
    )]
    pub enable_hiding: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ColumnRenderSpec {
    #[serde(rename = "type")]
    pub type_: ColumnRenderType,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ColumnRenderType {
    Text,
    Time,
    Link,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MenuSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placements: Vec<MenuPlacement>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MenuPlacement {
    Global,
    Workspace,
    Cluster,
}

#[derive(Debug, Snafu)]
pub enum ManifestRenderError {
    #[snafu(display(
        "FrontendIntegration {} has invalid routing.path '{}' (must not start with '/')",
        fi_name,
        path
    ))]
    InvalidRoutingPath { fi_name: String, path: String },
    #[snafu(display("FrontendIntegration {} requires columns for CRD integration", fi_name))]
    MissingCrdColumns { fi_name: String },
    #[snafu(display(
        "FrontendIntegration {} has invalid integration shape: type='{}' but corresponding field is missing",
        fi_name,
        integration_type
    ))]
    InvalidIntegrationShape {
        fi_name: String,
        integration_type: String,
    },
    #[snafu(display(
        "FrontendIntegration {} requested unsupported builder.engineVersion '{}'",
        fi_name,
        engine_version
    ))]
    UnsupportedEngineVersion {
        fi_name: String,
        engine_version: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "PascalCase")]
pub enum FrontendIntegrationPhase {
    #[default]
    Pending,
    Building,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct ResourceRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct ActiveBuildStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_ref: Option<ResourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct SimpleCondition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct FrontendIntegrationStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<FrontendIntegrationPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_spec_hash: Option<String>,
    // Deprecated compatibility field from earlier MVPs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_build: Option<ActiveBuildStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_ref: Option<ResourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<SimpleCondition>,
}

#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[kube(
    group = "extensions.kubesphere.io",
    version = "v1alpha1",
    kind = "JSBundle",
    plural = "jsbundles",
    status = "JsBundleStatus"
)]
pub struct JsBundleSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "rawFrom")]
    pub raw_from: Option<JsBundleRawFromSpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct JsBundleRawFromSpec {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "configMapKeyRef"
    )]
    pub config_map_key_ref: Option<JsBundleNamespacedKeyRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "secretKeyRef"
    )]
    pub secret_key_ref: Option<JsBundleNamespacedKeyRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct JsBundleNamespacedKeyRef {
    pub key: String,
    pub name: String,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct JsBundleStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Value>,
}

impl FrontendIntegrationSpec {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn without_enabled(&self) -> Self {
        let mut spec = self.clone();
        spec.enabled = None;
        spec
    }

    pub fn engine_version(&self) -> Option<&str> {
        self.builder
            .as_ref()
            .and_then(|builder| builder.engine_version.as_deref())
    }
}

impl MenuPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            MenuPlacement::Global => "global",
            MenuPlacement::Workspace => "workspace",
            MenuPlacement::Cluster => "cluster",
        }
    }

    pub fn route_prefix(self) -> &'static str {
        match self {
            MenuPlacement::Cluster => "/clusters/:cluster",
            MenuPlacement::Workspace => "/workspaces/:workspace",
            MenuPlacement::Global => "",
        }
    }
}
