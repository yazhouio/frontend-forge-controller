use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_GROUP: &str = "frontend-forge.io";
pub const API_VERSION: &str = "v1alpha1";
pub const JSBUNDLE_PLURAL: &str = "jsbundles";

#[derive(CustomResource, Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[kube(
    group = "frontend-forge.io",
    version = "v1alpha1",
    kind = "FrontendIntegration",
    plural = "frontendintegrations",
    namespaced,
    status = "FrontendIntegrationStatus",
    shortname = "fi"
)]
pub struct FrontendIntegrationSpec {
    pub source: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_rebuild_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
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
    pub observed_manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_force_rebuild_token: Option<String>,
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
    group = "frontend-forge.io",
    version = "v1alpha1",
    kind = "JSBundle",
    plural = "jsbundles",
    namespaced,
    status = "JsBundleStatus"
)]
pub struct JsBundleSpec {
    pub manifest_hash: String,
    #[serde(default)]
    pub files: Vec<JsBundleFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct JsBundleFile {
    pub path: String,
    pub encoding: JsBundleFileEncoding,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum JsBundleFileEncoding {
    Utf8,
    Base64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct JsBundleStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl FrontendIntegrationSpec {
    pub fn paused(&self) -> bool {
        self.paused.unwrap_or(false)
    }
}
