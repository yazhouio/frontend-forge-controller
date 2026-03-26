mod v1;

use frontend_forge_api::FrontendIntegration;
use kube::ResourceExt;
use serde_json::Value;
use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum ManifestRenderError {
    #[snafu(display(
        "FrontendIntegration {} has duplicate top-level menu key '{}'",
        fi_name,
        key
    ))]
    DuplicateTopLevelMenuKey { fi_name: String, key: String },
    #[snafu(display("FrontendIntegration {} has duplicate page key '{}'", fi_name, key))]
    DuplicatePageKey { fi_name: String, key: String },
    #[snafu(display(
        "FrontendIntegration {} is missing page config for menu key '{}'",
        fi_name,
        key
    ))]
    MissingPageForMenuKey { fi_name: String, key: String },
    #[snafu(display(
        "FrontendIntegration {} has page config '{}' without a menu binding",
        fi_name,
        key
    ))]
    OrphanPageConfig { fi_name: String, key: String },
    #[snafu(display(
        "FrontendIntegration {} has invalid menu shape for key '{}': {}",
        fi_name,
        key,
        message
    ))]
    InvalidMenuShape {
        fi_name: String,
        key: String,
        message: String,
    },
    #[snafu(display(
        "FrontendIntegration {} has invalid page shape for key '{}': {}",
        fi_name,
        key,
        message
    ))]
    InvalidPageShape {
        fi_name: String,
        key: String,
        message: String,
    },
    #[snafu(display("FrontendIntegration {} has invalid menu key '{}'", fi_name, key))]
    InvalidMenuKey { fi_name: String, key: String },
    #[snafu(display(
        "FrontendIntegration {} requires columns for CRD page '{}'",
        fi_name,
        key
    ))]
    MissingCrdColumns { fi_name: String, key: String },
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

// Rendering remains versioned so runner and webhook share the same validation semantics.
pub fn render_extension_manifest(fi: &FrontendIntegration) -> Result<Value, ManifestRenderError> {
    let requested = fi.spec.engine_version().unwrap_or("v1").trim();
    let normalized = if requested.is_empty() {
        "v1"
    } else {
        requested
    }
    .to_ascii_lowercase();

    match normalized.as_str() {
        "v1" | "v1alpha1" | "1" | "1.0" => v1::render_v1_manifest(fi),
        _ => Err(ManifestRenderError::UnsupportedEngineVersion {
            fi_name: fi.name_any(),
            engine_version: requested.to_string(),
        }),
    }
}

pub fn validate_frontend_integration(fi: &FrontendIntegration) -> Result<(), ManifestRenderError> {
    render_extension_manifest(fi).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml;

    #[test]
    fn defaults_to_v1_renderer() {
        let fi: FrontendIntegration = serde_yaml::from_str(
            r#"
apiVersion: frontend-forge.kubesphere.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: demo
spec:
  menus:
    - displayName: Demo
      key: demo
      placement: global
      type: page
  pages:
    - key: demo
      type: iframe
      iframe:
        src: http://example.test
"#,
        )
        .unwrap();

        let manifest = render_extension_manifest(&fi).unwrap();
        assert_eq!(manifest["version"], "1.0");
    }

    #[test]
    fn rejects_unknown_engine_version() {
        let fi: FrontendIntegration = serde_yaml::from_str(
            r#"
apiVersion: frontend-forge.kubesphere.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: demo
spec:
  builder:
    engineVersion: v99
  menus:
    - displayName: Demo
      key: demo
      placement: global
      type: page
  pages:
    - key: demo
      type: iframe
      iframe:
        src: http://example.test
"#,
        )
        .unwrap();

        assert!(matches!(
            render_extension_manifest(&fi),
            Err(ManifestRenderError::UnsupportedEngineVersion { .. })
        ));
    }

    #[test]
    fn validate_frontend_integration_reuses_render_path() {
        let fi: FrontendIntegration = serde_yaml::from_str(
            r#"
apiVersion: frontend-forge.kubesphere.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: demo
spec:
  menus:
    - displayName: Demo
      key: demo
      placement: global
      type: page
  pages:
    - key: demo
      type: iframe
      iframe:
        src: http://example.test
"#,
        )
        .unwrap();

        assert!(validate_frontend_integration(&fi).is_ok());
    }

    #[test]
    fn validate_frontend_integration_returns_domain_errors() {
        let fi: FrontendIntegration = serde_yaml::from_str(
            r#"
apiVersion: frontend-forge.kubesphere.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: demo
spec:
  menus:
    - displayName: Demo
      key: demo
      placement: global
      type: page
  pages:
    - key: demo
      type: iframe
      iframe:
        src: http://example.test
    - key: demo
      type: iframe
      iframe:
        src: http://example.test/other
"#,
        )
        .unwrap();

        assert!(matches!(
            validate_frontend_integration(&fi),
            Err(ManifestRenderError::DuplicatePageKey { .. })
        ));
    }
}
