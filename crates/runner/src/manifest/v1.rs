use frontend_forge_api::{
    ColumnRenderType, ColumnSpec, CrdIntegrationSpec, FrontendIntegration, FrontendIntegrationSpec,
    IntegrationType, ManifestRenderError, MenuPlacement,
};
use kube::ResourceExt;
use serde_json::{Map, Value, json};

pub(super) fn render_v1_manifest(fi: &FrontendIntegration) -> Result<Value, ManifestRenderError> {
    let fi_name = fi.name_any();
    let routing_path = fi.spec.routing.path.trim();
    if routing_path.is_empty() || routing_path.starts_with('/') {
        return Err(ManifestRenderError::InvalidRoutingPath {
            fi_name,
            path: fi.spec.routing.path.clone(),
        });
    }

    let fi_name = fi.name_any();
    let display_name = fi
        .spec
        .display_name
        .clone()
        .unwrap_or_else(|| fi_name.clone());
    let description = fi
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("kubesphere.io/description").cloned());
    let placements = effective_placements(&fi.spec);

    let route_tail = format!("/frontendintegrations/{}/{}", fi_name, routing_path);
    let routes: Vec<Value> = placements
        .iter()
        .map(|placement| {
            let page_id = page_id(&fi_name, *placement);
            let path = format!("{}{}", placement.route_prefix(), route_tail);
            json!({
                "path": path,
                "pageId": page_id,
            })
        })
        .collect();

    let menus: Vec<Value> = if fi.spec.menu.is_some() {
        let menu_title = fi
            .spec
            .menu
            .as_ref()
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| display_name.clone());
        placements
            .iter()
            .map(|placement| {
                json!({
                    "parent": placement.as_str(),
                    "name": format!("frontendintegrations/{}/{}", fi_name, routing_path),
                    "title": menu_title,
                    "icon": "GridDuotone",
                    "order": 999,
                })
            })
            .collect()
    } else {
        vec![]
    };

    let pages = match fi.spec.integration.type_ {
        IntegrationType::Iframe => {
            let iframe = fi.spec.integration.iframe.as_ref().ok_or_else(|| {
                ManifestRenderError::InvalidIntegrationShape {
                    fi_name: fi_name.clone(),
                    integration_type: "iframe".to_string(),
                }
            })?;
            placements
                .iter()
                .map(|placement| iframe_page(&fi_name, &display_name, *placement, &iframe.src))
                .collect::<Vec<_>>()
        }
        IntegrationType::Crd => {
            let crd = fi.spec.integration.crd.as_ref().ok_or_else(|| {
                ManifestRenderError::InvalidIntegrationShape {
                    fi_name: fi_name.clone(),
                    integration_type: "crd".to_string(),
                }
            })?;
            let columns = if !fi.spec.columns.is_empty() {
                fi.spec.columns.clone()
            } else {
                crd.columns.clone()
            };
            if columns.is_empty() {
                return Err(ManifestRenderError::MissingCrdColumns { fi_name });
            }
            placements
                .iter()
                .map(|placement| crd_page(&fi_name, &display_name, *placement, crd, &columns))
                .collect::<Vec<_>>()
        }
    };

    let mut manifest = Map::new();
    manifest.insert("version".to_string(), json!("1.0"));
    manifest.insert("name".to_string(), json!(fi_name));
    manifest.insert("displayName".to_string(), json!(display_name));
    if let Some(description) = description {
        manifest.insert("description".to_string(), json!(description));
    }
    manifest.insert("routes".to_string(), Value::Array(routes));
    manifest.insert("menus".to_string(), Value::Array(menus));
    manifest.insert("locales".to_string(), json!([]));
    manifest.insert("pages".to_string(), Value::Array(pages));
    manifest.insert(
        "build".to_string(),
        json!({
            "target": "kubesphere-extension",
            "moduleName": fi.name_any(),
            "systemjs": true,
        }),
    );

    Ok(Value::Object(manifest))
}

fn effective_placements(spec: &FrontendIntegrationSpec) -> Vec<MenuPlacement> {
    let placements = spec
        .menu
        .as_ref()
        .map(|m| m.placements.clone())
        .unwrap_or_default();
    if placements.is_empty() {
        vec![MenuPlacement::Global]
    } else {
        placements
    }
}

fn page_id(fi_name: &str, placement: MenuPlacement) -> String {
    format!("{}-{}", fi_name, placement.as_str())
}

fn page_meta(page_id: &str, title: &str) -> Value {
    json!({
      "id": page_id,
      "name": page_id,
      "title": title,
      "path": format!("/{}", page_id),
    })
}

fn iframe_page(
    fi_name: &str,
    display_name: &str,
    placement: MenuPlacement,
    frame_src: &str,
) -> Value {
    let page_id = page_id(fi_name, placement);
    json!({
      "id": page_id,
      "entryComponent": page_id,
      "componentsTree": {
        "meta": page_meta(&page_id, display_name),
        "context": {},
        "root": {
          "id": format!("{}-root", page_id),
          "type": "Iframe",
          "props": {
            "FRAME_URL": frame_src,
          },
          "meta": { "title": "Iframe", "scope": true }
        }
      }
    })
}

fn crd_page(
    fi_name: &str,
    display_name: &str,
    placement: MenuPlacement,
    crd: &CrdIntegrationSpec,
    columns: &[ColumnSpec],
) -> Value {
    let page_id = page_id(fi_name, placement);
    let scope_str = placement.as_str();
    let columns_config = transform_columns(columns);

    json!({
      "id": page_id,
      "entryComponent": page_id,
      "componentsTree": {
        "meta": page_meta(&page_id, display_name),
        "context": {},
        "dataSources": [
          {
            "id": "columns",
            "type": "crd-columns",
            "config": {
              "COLUMNS_CONFIG": columns_config,
              "HOOK_NAME": "useCrdColumns"
            }
          },
          {
            "id": "pageState",
            "type": "crd-page-state",
            "args": [
              { "type": "binding", "source": "columns", "bind": "columns" }
            ],
            "config": {
              "PAGE_ID": page_id,
              "CRD_CONFIG": {
                "apiVersion": crd.version,
                "kind": crd.names.kind,
                "plural": crd.names.plural,
                "group": crd.group,
                "kapi": true
              },
              "SCOPE": scope_str,
              "HOOK_NAME": "useCrdPageState"
            }
          }
        ],
        "root": {
          "id": format!("{}-root", page_id),
          "type": "CrdTable",
          "props": {
            "TABLE_KEY": page_id,
            "TITLE": display_name,
            "PARAMS": { "type": "binding", "source": "pageState", "bind": "params" },
            "REFETCH": { "type": "binding", "source": "pageState", "bind": "refetch" },
            "TOOLBAR_LEFT": { "type": "binding", "source": "pageState", "bind": "toolbarLeft" },
            "PAGE_CONTEXT": { "type": "binding", "source": "pageState", "bind": "pageContext" },
            "COLUMNS": { "type": "binding", "source": "columns", "bind": "columns" },
            "DATA": { "type": "binding", "source": "pageState", "bind": "data" },
            "IS_LOADING": {
              "type": "binding",
              "source": "pageState",
              "bind": "loading",
              "defaultValue": false
            },
            "UPDATE": { "type": "binding", "source": "pageState", "bind": "update" },
            "DEL": { "type": "binding", "source": "pageState", "bind": "del" },
            "CREATE": { "type": "binding", "source": "pageState", "bind": "create" },
            "CREATE_INITIAL_VALUE": {
              "apiVersion": format!("{}/{}", crd.group, crd.version),
              "kind": crd.names.kind
            }
          },
          "meta": { "title": "CrdTable", "scope": true }
        }
      }
    })
}

fn transform_columns(columns: &[ColumnSpec]) -> Vec<Value> {
    columns
        .iter()
        .map(|col| {
            let mut payload = payload_object(col.render.payload.as_ref());
            if let Some(format) = &col.render.format {
                payload.insert("format".to_string(), json!(format));
            }
            if let Some(pattern) = &col.render.pattern {
                payload.insert("pattern".to_string(), json!(pattern));
            }
            if let Some(link) = &col.render.link {
                payload.insert("link".to_string(), json!(link));
            }

            let mut out = Map::new();
            out.insert("key".to_string(), json!(col.key));
            out.insert("title".to_string(), json!(col.title));
            out.insert(
                "render".to_string(),
                json!({
                  "type": render_type_str(&col.render.type_),
                  "path": col.render.path,
                  "payload": Value::Object(payload),
                }),
            );
            if let Some(v) = col.enable_sorting {
                out.insert("enableSorting".to_string(), json!(v));
            }
            if let Some(v) = col.enable_hiding {
                out.insert("enableHiding".to_string(), json!(v));
            }
            Value::Object(out)
        })
        .collect()
}

fn payload_object(payload: Option<&Value>) -> Map<String, Value> {
    match payload {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn render_type_str(t: &ColumnRenderType) -> &'static str {
    match t {
        ColumnRenderType::Text => "text",
        ColumnRenderType::Time => "time",
        ColumnRenderType::Link => "link",
    }
}
