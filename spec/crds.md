# FrontendIntegration CRD 定义

## 1. 设计目标

`FrontendIntegration` CRD 用于**声明前端扩展的用户意图**，其核心目标是：

- 以 Kubernetes 风格描述一个前端扩展能力
- 表达「**集成类型 + 路由 + 菜单**」等高层语义
- 不关心前端工程实现细节
- 作为 **Scene 生成的唯一输入源**

该 CRD **不直接参与前端构建**，而是由 Node.js（v1）或 Controller（v2）解释并转化为 Scene。

---

## 2. CRD 基本信息

```yaml
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: frontendintegrations.frontend-forge.io
spec:
  group: frontend-forge.io
  scope: Namespaced
  names:
    plural: frontendintegrations
    singular: frontendintegration
    kind: FrontendIntegration
    shortNames:
      - fi
```

---

## 3. Spec 结构定义（CRD Schema）

### 3.1 完整结构

```yaml
spec:
  versions:
    - name: v1alpha1
      served: true
      storage: true
      schema:
        openAPIV3Schema:
          type: object
          additionalProperties: false
          required:
            - spec
          properties:
            apiVersion:
              type: string
            kind:
              type: string
            metadata:
              type: object

            spec:
              type: object
              additionalProperties: false
              required:
                - integration
                - routing
                - columns
              properties:
                displayName:
                  type: string
                  description: Human-readable name for UI display
                enabled:
                  type: boolean
                  description: Whether this integration is active (true) or disabled (false)

                integration:
                  type: object
                  additionalProperties: false
                  oneOf:
                    # ---- CRD 集成模式 ----
                    - required: [type, crd]
                      properties:
                        type:
                          type: string
                          enum: [crd]
                        crd:
                          type: object
                          additionalProperties: false
                          required:
                            - names
                            - group
                            - version
                            - scope
                          properties:
                            names:
                              type: object
                              additionalProperties: false
                              required:
                                - kind
                                - plural
                              properties:
                                kind:
                                  type: string
                                plural:
                                  type: string

                            group:
                              type: string

                            version:
                              type: string

                            authKey:
                              type: string

                            scope:
                              type: string
                              enum:
                                - Namespaced
                                - Cluster

                    # ---- Iframe 集成模式 ----
                    - required: [type, iframe]
                      properties:
                        type:
                          type: string
                          enum: [iframe]
                        iframe:
                          type: object
                          additionalProperties: false
                          required:
                            - src
                          properties:
                            src:
                              type: string

                routing:
                  type: object
                  additionalProperties: false
                  required:
                    - path
                  properties:
                    path:
                      type: string
                      description: Relative route path, without leading slash
                      pattern: "^[^/].*$"

                columns:
                  type: array
                  description: Table columns for CRD list view
                  items:
                    type: object
                    additionalProperties: false
                    required:
                      - key
                      - title
                      - render
                    properties:
                      key:
                        type: string
                      title:
                        type: string
                      render:
                        type: object
                        additionalProperties: false
                        required:
                          - type
                          - path
                        properties:
                          type:
                            type: string
                            enum: [text, time, link]
                          path:
                            type: string
                          format:
                            type: string
                            enum: [local-datetime, utc]
                          pattern:
                            type: string
                          link:
                            type: string
                          payload:
                            type: object
                            additionalProperties: true

                menu:
                  type: object
                  additionalProperties: false
                  properties:
                    name:
                      type: string
                    placements:
                      type: array
                      items:
                        type: string
```

---

### 3.2 integration 字段（核心）

`integration` 描述该前端扩展**如何与后端 / 集群资源集成**。
该字段通过 `type` 区分不同集成模型，并使用 `oneOf` 进行严格约束。

```yaml
integration:
  oneOf:
    - required: [type, crd]
      properties:
        type:
          enum: [crd]
        crd:
          type: object
          additionalProperties: false
          required:
            - names
            - group
            - version
            - scope
          properties:
            names:
              type: object
              additionalProperties: false
              required:
                - kind
                - plural
              properties:
                kind:
                  type: string
                plural:
                  type: string

            group:
              type: string

            version:
              type: string

            authKey:
              type: string

            scope:
              type: string
              enum:
                - Namespaced
                - Cluster

    - required: [type, iframe]
      properties:
        type:
          enum: [iframe]
        iframe:
          type: object
          additionalProperties: false
          required:
            - src
          properties:
            src:
              type: string
```

---

## 4. 字段语义说明

### 4.1 displayName（新增）

```yaml
spec:
  displayName: "CRD 资源管理"
```

- 面向用户的展示名称
- **不要求唯一**
- 可随时修改，不影响资源 identity
- 不参与构建 / 调度决策
- 可作为 i18n key 或 fallback 文本

---

### 4.2 enabled（新增）

```yaml
spec:
  enabled: true
```

- `true` 表示已启动（可被解析并生成 Scene）
- `false` 表示已禁用（不参与 Scene 生成 / 展示）
- 默认值建议为 `true`

---

### 4.3 metadata.name（保持不变）

```yaml
metadata:
  name: workspace-crd-table
```

- 系统级唯一标识
- 用于：
  - Controller 关联
  - JSBundle 命名
  - Scene 生成的稳定 key

---

### 4.4 routing.path（相对路径）

```yaml
routing:
  path: crds
```

语义约定：

- **不以 `/` 开头**
- 表示页面在当前 Scene / 容器下的相对路径
- 完整路由由 Scene 生成，例如：

```text
/workspaces/:workspace + /crds
→ /workspaces/:workspace/crds
```

### 4.5 integration.type

```yaml
integration:
  type: crd | iframe
```

- 决定 Scene 的选择
- 是 Controller / Node.js 的**主分支判断条件**
- 新的集成模型可通过新增枚举扩展（如 `wasm` / `remote`）

---

### 4.6 integration.crd

用于声明一个 **基于 Kubernetes 资源的前端扩展**：

```yaml
integration:
  type: crd
  crd:
    names:
      kind: Workspace
      plural: workspaces
    group: tenant.kubesphere.io
    version: v1alpha2
    authKey: workspace-auth
    scope: Namespaced
```

语义：

- `names`：对应 Kubernetes 资源命名信息（如 `kind` / `plural`）
- `group` / `version`：用于 discovery 与 REST 路径拼装
- `authKey`：可选鉴权标识，用于选择对应的访问凭据
- `scope`：决定 namespace / cluster 行为

---

### 4.7 integration.iframe

```yaml
integration:
  type: iframe
  iframe:
    src: /plugins/example/index.html
```

语义：

- 表示一个完全外部托管的前端页面
- Scene 通常为 `IframeScene`
- 不参与数据建模，仅作为 UI 容器

---

### 4.8 columns（必填）

```yaml
columns:
  - key: name
    title: "名称"
    render:
      type: text
      path: metadata.name
  - key: updateTime
    title: "创建时间"
    render:
      type: time
      path: metadata.creationTimestamp
      format: local-datetime
  - key: link
    title: "详情"
    render:
      type: link
      path: metadata.name
      link: "/workspaces/:workspace/crds/{metadata.name}"
      payload:
        target: "_blank"
```

- 描述 CRD 列表页的表格列（必填）
- `key`：列唯一标识
- `title`：列标题
- `render`：渲染规则
  - `type`：`text` / `time` / `link`
  - `path`：数据路径（JSONPath-like）
  - `format`：时间格式化策略（仅 `time` 类型生效）
  - `pattern`：可选格式模板（如自定义日期或字符串格式）
  - `link`：链接模板（`link` 类型生效，可包含占位）
  - `payload`：透传渲染参数

---

### 4.9 menu（可选）

```yaml
menu:
  name: "CRD 管理"
  placements:
    - workspace
```

- 描述扩展在 UI 中的入口位置
- `placements` 建议枚举化（如 `global` / `workspace` / `cluster`）

---

## 5. 示例 CR（完整）

```yaml
apiVersion: frontend-forge.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: workspace-crd-table
  annotations:
    kubesphere.io/description: "Workspace 级别 CRD 列表与管理页面"
spec:
  displayName: "CRD 资源管理"
  enabled: true
  integration:
    type: crd
    crd:
      names:
        kind: Workspace
        plural: workspaces
      group: tenant.kubesphere.io
      version: v1alpha2
      authKey: workspace-auth
      scope: Namespaced

  routing:
    path: crds

  columns:
    - key: name
      title: "名称"
      render:
        type: text
        path: metadata.name
    - key: created
      title: "创建时间"
      render:
        type: time
        path: metadata.creationTimestamp
        format: local-datetime

  menu:
    name: "CRD 管理"
    placements:
      - workspace
      - cluster
```

---

## 6. v1 / v2 中 CRD 的使用差异

### v1

- Node.js 直接接收并解析 `FrontendIntegration`
- 在 Node.js 内部生成 `Scene[]`
- Scene 配置写入 `JSBundle.metadata.annotations["scene.frontend-forge.io/config"]`

---

### v2

- Controller watch `FrontendIntegration`
- 校验与收敛 spec
- 生成 **Scene[] ConfigMap**
- Node.js **不再解析 CRD**

---

## 7. 设计总结

> **FrontendIntegration CRD 只表达“用户想要什么前端扩展”，
> 而不表达“如何构建、如何运行”。
> 它是前端控制面的意图层，而不是执行层。**

---
