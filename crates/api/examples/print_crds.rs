use frontend_forge_api::frontend_integration_crd;

fn main() -> Result<(), serde_yaml::Error> {
    let fi = frontend_integration_crd();
    // JSBundle is a third-party CRD (extensions.kubesphere.io) and is not generated here.
    println!("{}", serde_yaml::to_string(&fi)?);
    Ok(())
}
