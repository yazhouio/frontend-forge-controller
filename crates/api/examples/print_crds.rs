use frontend_forge_api::{FrontendIntegration, JSBundle};
use kube::CustomResourceExt;

fn main() {
    let fi = FrontendIntegration::crd();
    let bundle = JSBundle::crd();

    println!(
        "{}---",
        serde_yaml::to_string(&fi).expect("serialize FI CRD")
    );
    println!(
        "{}",
        serde_yaml::to_string(&bundle).expect("serialize JSBundle CRD")
    );
}
