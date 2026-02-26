# Manifest 定义

```typescript
export type RouteMeta = {
  path: string;
  pageId: string;
};

export type MenuMeta = {
  parent: string;
  name: string;
  title: string;
  icon?: string;
  order?: number;
  clusterModule?: string;
};

export type LocaleMeta = {
  lang: string;
  messages: Record<string, string>;
};

export type ManifestPageMeta = {
  id: string;
  entryComponent: string;
  componentsTree: PageConfig;
};

export type ExtensionManifest = {
  version: "1.0";
  name: string;
  displayName?: string;
  description?: string;
  routes: RouteMeta[];
  menus: MenuMeta[];
  locales: LocaleMeta[];
  pages: ManifestPageMeta[];
  build?: {
    target: "kubesphere-extension";
    moduleName?: string;
    namespace?: string;
    cluster?: string;
    systemjs?: boolean;
  };
};

export interface PageConfig {
  meta: PageConfigMeta;
  dataSources?: DataSourceNode[];
  root: ComponentNode;
  context: Record<string, any>;
}

export interface PageConfigMeta {
  id: string;
  name: string;
  title?: string;
  description?: string;
  path?: string;
}

export interface DataSourceNode {
  id: string;
  type: string;
  config: Record<string, any>;
  args?: PropValue[];
  autoLoad?: boolean;
  polling?: {
    enabled: boolean;
    interval?: number;
  };
}

export interface ComponentNode {
  id: string;
  type: string;
  props?: Record<string, PropValue>;
  meta?: {
    scope: boolean;
    title?: string;
  };
  children?: ComponentNode[];
}

export type PropValue =
  | string
  | number
  | boolean
  | object
  | BindingValue
  | ExpressionValue;

export interface BindingValue {
  type: "binding";
  source?: string;
  bind?: string;
  target?: "context" | "dataSource" | "runtime";
  path?: string;
  defaultValue?: any;
}

export interface ExpressionValue {
  type: "expression";
  code: string;
  deps?: ExpressionDeps;
}

export interface ExpressionDeps {
  dataSources?: string[];
  runtime?: true;
  capabilities?: string[];
}
```

### demo 1， iframe cr 生成下面 manifest

```yaml
apiVersion: frontend-forge.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: sss
  annotations:
    kubesphere.io/description: sss
    kubesphere.io/creator: admin
  creationTimestamp: "2026-02-11T06:29:32Z"
spec:
  enabled: true
  integration:
    type: iframe
    iframe:
      url: >-
        http://139.198.121.90:40880/clusters/host/frontendintegrations/servicemonitors1/asdfas
    menu:
      name: weww
  menu:
    placements:
      - cluster
      - workspace
      - global
  routing:
    path: wewew
```

```json
{
  "version": "1.0",
  "name": "sss",
  "displayName": "sss",
  "routes": [
    {
      "path": "/clusters/:cluster/frontendintegrations/sss/wewew",
      "pageId": "sss-cluster"
    },
    {
      "path": "/workspaces/:workspace/frontendintegrations/sss/wewew",
      "pageId": "sss-workspace"
    },
    { "path": "/frontendintegrations/sss/wewew", "pageId": "sss-global" }
  ],
  "menus": [
    {
      "parent": "cluster",
      "name": "frontendintegrations/sss/wewew",
      "title": "sss",
      "icon": "GridDuotone",
      "order": 999
    },
    {
      "parent": "workspace",
      "name": "frontendintegrations/sss/wewew",
      "title": "sss",
      "icon": "GridDuotone",
      "order": 999
    },
    {
      "parent": "global",
      "name": "frontendintegrations/sss/wewew",
      "title": "sss",
      "icon": "GridDuotone",
      "order": 999
    }
  ],
  "locales": [],
  "pages": [
    {
      "id": "sss-cluster",
      "entryComponent": "sss-cluster",
      "componentsTree": {
        "meta": {
          "id": "sss-cluster",
          "name": "sss-cluster",
          "title": "sss",
          "path": "/sss-cluster"
        },
        "context": {},
        "root": {
          "id": "sss-cluster-root",
          "type": "Iframe",
          "props": {
            "FRAME_URL": "http://139.198.121.90:40880/clusters/host/frontendintegrations/servicemonitors1/asdfas"
          },
          "meta": { "title": "Iframe", "scope": true }
        }
      }
    },
    {
      "id": "sss-workspace",
      "entryComponent": "sss-workspace",
      "componentsTree": {
        "meta": {
          "id": "sss-workspace",
          "name": "sss-workspace",
          "title": "sss",
          "path": "/sss-workspace"
        },
        "context": {},
        "root": {
          "id": "sss-workspace-root",
          "type": "Iframe",
          "props": {
            "FRAME_URL": "http://139.198.121.90:40880/clusters/host/frontendintegrations/servicemonitors1/asdfas"
          },
          "meta": { "title": "Iframe", "scope": true }
        }
      }
    },
    {
      "id": "sss-global",
      "entryComponent": "sss-global",
      "componentsTree": {
        "meta": {
          "id": "sss-global",
          "name": "sss-global",
          "title": "sss",
          "path": "/sss-global"
        },
        "context": {},
        "root": {
          "id": "sss-global-root",
          "type": "Iframe",
          "props": {
            "FRAME_URL": "http://139.198.121.90:40880/clusters/host/frontendintegrations/servicemonitors1/asdfas"
          },
          "meta": { "title": "Iframe", "scope": true }
        }
      }
    }
  ],
  "build": {
    "target": "kubesphere-extension",
    "moduleName": "sss",
    "systemjs": true
  }
}
```

### demo 2， crd 集成

```yaml
apiVersion: frontend-forge.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: qweqwcccc
  annotations:
    kubesphere.io/creator: admin
  creationTimestamp: "2026-02-24T07:42:15Z"
spec:
  enabled: true
  integration:
    type: crd
    crd:
      columns:
        - key: name
          title: NAME
          enableSorting: true
          render:
            type: text
            path: metadata.name
        - key: updateTime
          title: CREATION_TIME
          enableHiding: true
          enableSorting: true
          render:
            type: time
            path: metadata.creationTimestamp
            format: local-datetime
      names:
        plural: inspectrules
        kind: InspectRule
      version: v1alpha2
      group: kubeeye.kubesphere.io
      scope: Cluster
    menu:
      name: "2e22"
  menu:
    placements:
      - cluster
  routing:
    path: e2e2
```

生成下面 manifest

```json
{
  "version": "1.0",
  "name": "qweqwcccc",
  "displayName": "qweqwcccc",
  "routes": [
    {
      "path": "/clusters/:cluster/frontendintegrations/qweqwcccc/e2e2",
      "pageId": "qweqwcccc-cluster"
    }
  ],
  "menus": [
    {
      "parent": "cluster",
      "name": "frontendintegrations/qweqwcccc/e2e2",
      "title": "qweqwcccc",
      "icon": "GridDuotone",
      "order": 999
    }
  ],
  "locales": [],
  "pages": [
    {
      "id": "qweqwcccc-cluster",
      "entryComponent": "qweqwcccc-cluster",
      "componentsTree": {
        "meta": {
          "id": "qweqwcccc-cluster",
          "name": "qweqwcccc-cluster",
          "title": "qweqwcccc",
          "path": "/qweqwcccc-cluster"
        },
        "context": {},
        "dataSources": [
          {
            "id": "columns",
            "type": "crd-columns",
            "config": {
              "COLUMNS_CONFIG": [
                {
                  "key": "name",
                  "title": "NAME",
                  "render": {
                    "type": "text",
                    "path": "metadata.name",
                    "payload": {}
                  },
                  "enableSorting": true
                },
                {
                  "key": "updateTime",
                  "title": "CREATION_TIME",
                  "render": {
                    "type": "time",
                    "path": "metadata.creationTimestamp",
                    "payload": { "format": "local-datetime" }
                  },
                  "enableHiding": true,
                  "enableSorting": true
                }
              ],
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
              "PAGE_ID": "qweqwcccc-cluster",
              "CRD_CONFIG": {
                "apiVersion": "v1alpha2",
                "kind": "InspectRule",
                "plural": "inspectrules",
                "group": "kubeeye.kubesphere.io",
                "kapi": true
              },
              "SCOPE": "cluster",
              "HOOK_NAME": "useCrdPageState"
            }
          }
        ],
        "root": {
          "id": "qweqwcccc-cluster-root",
          "type": "CrdTable",
          "props": {
            "TABLE_KEY": "qweqwcccc-cluster",
            "TITLE": "qweqwcccc",
            "PARAMS": {
              "type": "binding",
              "source": "pageState",
              "bind": "params"
            },
            "REFETCH": {
              "type": "binding",
              "source": "pageState",
              "bind": "refetch"
            },
            "TOOLBAR_LEFT": {
              "type": "binding",
              "source": "pageState",
              "bind": "toolbarLeft"
            },
            "PAGE_CONTEXT": {
              "type": "binding",
              "source": "pageState",
              "bind": "pageContext"
            },
            "COLUMNS": {
              "type": "binding",
              "source": "columns",
              "bind": "columns"
            },
            "DATA": {
              "type": "binding",
              "source": "pageState",
              "bind": "data"
            },
            "IS_LOADING": {
              "type": "binding",
              "source": "pageState",
              "bind": "loading",
              "defaultValue": false
            },
            "UPDATE": {
              "type": "binding",
              "source": "pageState",
              "bind": "update"
            },
            "DEL": { "type": "binding", "source": "pageState", "bind": "del" },
            "CREATE": {
              "type": "binding",
              "source": "pageState",
              "bind": "create"
            },
            "CREATE_INITIAL_VALUE": {
              "apiVersion": "kubeeye.kubesphere.io/v1alpha2",
              "kind": "InspectRule"
            }
          },
          "meta": { "title": "CrdTable", "scope": true }
        }
      }
    }
  ],
  "build": {
    "target": "kubesphere-extension",
    "moduleName": "qweqwcccc",
    "systemjs": true
  }
}
```
