use frontend_forge_api::FrontendIntegration;
use kube::CustomResourceExt;

fn main() {
    let fi = FrontendIntegration::crd();
    // JSBundle is a third-party CRD (extensions.kubesphere.io) and is not generated here.
    println!("{}", serde_yaml::to_string(&fi).expect("serialize FI CRD"));
}
