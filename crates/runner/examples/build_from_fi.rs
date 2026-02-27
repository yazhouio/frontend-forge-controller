#[path = "../src/manifest.rs"]
mod manifest;

use frontend_forge_api::FrontendIntegration;
use frontend_forge_common::manifest_content_and_hash;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

type DynError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Deserialize)]
struct ProjectBuildResponse {
    ok: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    files: Vec<RemoteFile>,
}

#[derive(Debug, Deserialize)]
struct RemoteFile {
    path: String,
    content: String,
}

struct CliArgs {
    fi_yaml_path: PathBuf,
    base_url: String,
    output_dir: PathBuf,
    timeout_seconds: u64,
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let args = parse_args()?;
    let fi_text = fs::read_to_string(&args.fi_yaml_path)?;
    let fi: FrontendIntegration = serde_yaml::from_str(&fi_text)?;
    let manifest_value = manifest::render_extension_manifest(&fi)?;
    let (manifest_content, manifest_hash) = manifest_content_and_hash(&manifest_value)?;

    fs::create_dir_all(&args.output_dir)?;
    let manifest_path = args.output_dir.join("manifest.json");
    fs::write(&manifest_path, &manifest_content)?;

    let request_url = format!("{}/api/project/build", args.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout_seconds))
        .build()?;
    let resp = client
        .post(&request_url)
        .header(CONTENT_TYPE, "application/json")
        .body(manifest_content.clone())
        .send()
        .await?
        .error_for_status()?;
    let payload: ProjectBuildResponse = resp.json().await?;
    let response_path = args.output_dir.join("build_response.json");
    fs::write(
        &response_path,
        serde_json::to_string_pretty(&json!({
            "ok": payload.ok,
            "message": payload.message,
            "files_count": payload.files.len(),
            "manifest_hash": manifest_hash,
            "request_url": request_url
        }))?,
    )?;

    if !payload.ok {
        let msg = payload
            .message
            .unwrap_or_else(|| "build-service returned ok=false".to_string());
        return Err(msg.into());
    }

    for file in payload.files {
        let rel = safe_relative_path(&file.path)?;
        let target = args.output_dir.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, file.content)?;
    }

    println!("manifest: {}", manifest_path.display());
    println!("response: {}", response_path.display());
    println!("files dir: {}", args.output_dir.display());
    Ok(())
}

fn parse_args() -> Result<CliArgs, DynError> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 2 || args.len() > 3 {
        return Err(
            "usage: cargo run -p frontend-forge-runner --example build_from_fi -- <demo_fi.yaml> <build_service_base_url> [output_dir]".into(),
        );
    }

    let timeout_seconds = env::var("BUILD_SERVICE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(600);

    Ok(CliArgs {
        fi_yaml_path: PathBuf::from(&args[0]),
        base_url: args[1].clone(),
        output_dir: args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/runner-example-output")),
        timeout_seconds,
    })
}

fn safe_relative_path(raw: &str) -> Result<PathBuf, DynError> {
    let src = Path::new(raw);
    if src.is_absolute() {
        return Err(format!("artifact path must be relative: {raw}").into());
    }

    let mut out = PathBuf::new();
    for component in src.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("artifact path contains unsafe component: {raw}").into())
            }
        }
    }

    if out.as_os_str().is_empty() {
        return Err(format!("artifact path is empty: {raw}").into());
    }
    Ok(out)
}
