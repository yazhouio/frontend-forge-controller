use frontend_forge_api::FrontendIntegration;
use kube::CustomResourceExt;
use std::collections::BTreeMap;

fn main() -> Result<(), serde_yaml::Error> {
    let mut fi = FrontendIntegration::crd();
    fi.metadata.labels.get_or_insert_with(BTreeMap::new).insert(
        "kubesphere.io/resource-served".to_string(),
        "true".to_string(),
    );
    // JSBundle is a third-party CRD (extensions.kubesphere.io) and is not generated here.
    println!("{}", serde_yaml::to_string(&fi)?);
    Ok(())
}
