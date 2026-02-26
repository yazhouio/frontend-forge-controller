# Frontend Forge Builder Controller 设计文档（当前实现对齐）

## 1. 目标与当前实现结论

当前实现以 `FrontendIntegration`（FI）作为唯一用户入口，`Job` 作为一次性 runner，`JSBundle` 作为产物 CR。

与早期方案相比，当前代码已经收敛为以下模型：

1. Controller **不再渲染 Manifest**，也不依赖 Manifest `ConfigMap/Secret`
2. Controller 的幂等与构建触发基于 **FI.spec 的 canonical hash（spec_hash）**
3. Runner 在 Job 内部读取 FI，并按 `spec.builder.engineVersion` 将 FI 转换为 Manifest
4. Runner 计算 **manifest_hash**，用于 build-service 调用与 `JSBundle.spec.manifest_hash`
5. Controller 通过 `JSBundle` 的 `spec-hash` label 判断产物是否属于当前 FI 期望版本

因此，当前实现存在两个 hash（职责不同）：

- `spec_hash`：控制面幂等 / Job 身份 / stale-check 对齐
- `manifest_hash`：构建输入（Manifest）追溯 / build-service 参数 / JSBundle 内容追溯

## 2. 资源与职责

### 2.1 FrontendIntegration（唯一用户 CR）

- 用户唯一入口
- 表达前端扩展的高层语义（集成类型、路由、列配置、菜单等）
- Controller 只基于 `FI.spec` 计算 `spec_hash`
- Runner 基于 `FI` 渲染 Manifest

当前已实现的重要字段（节选）：

- `spec.displayName`
- `spec.enabled`
- `spec.integration`（`crd` / `iframe`）
- `spec.routing.path`
- `spec.columns`
- `spec.menu`
- `spec.builder.engineVersion`（新增，用于选择 runner 侧转换版本）

### 2.2 Job（一次性 runner）

- 一次构建执行单元
- 输入为 FI 引用 + `SPEC_HASH`（而不是 manifest 文件）
- 职责：
  - 获取 FI
  - 校验 `FI.spec` 的 hash 是否仍等于 `SPEC_HASH`
  - 按 engine version 生成 Manifest
  - 调用 build-service
  - stale-check 后创建/更新 `JSBundle`

### 2.3 JSBundle（产物 CR）

- 固定名称更新（当前代码使用 `fi-<fi-name>`）
- 由 runner 创建/更新
- 存储：
  - `spec.manifest_hash`
  - `spec.files`
- metadata labels 同时记录 `spec_hash` 与 `manifest_hash`（用于 controller 判定与追溯）

## 3. OwnerReference 与状态引用

当前实现的 owner 关系：

- `Job.ownerReference -> FrontendIntegration`
- `JSBundle.ownerReference -> FrontendIntegration`

说明：

- 当前实现路径中 **没有 Manifest Secret/ConfigMap**
- Controller 也不再需要 `Secret` RBAC 权限

`FI.status` 当前实现字段（节选）：

- `status.phase`
- `status.observed_spec_hash`
- `status.observed_manifest_hash`（兼容/追溯）
- `status.active_build.job_ref`
- `status.bundle_ref`
- `status.message`

## 4. 双 Hash 模型（当前实现）

### 4.1 `spec_hash`（Controller 使用）

定义：

- `spec_hash = sha256(canonical_json(FI.spec))`

用途：

- Controller 幂等判定（是否需要新构建）
- Job 标签筛选（查找同版本 Job）
- Job 环境变量 `SPEC_HASH`
- Runner stale-check 对齐（与 `FI.status.observed_spec_hash` 比较）
- `JSBundle` label `frontend-forge.io/spec-hash`

### 4.2 `manifest_hash`（Runner 使用）

定义：

- `manifest_hash = sha256(canonical_json(rendered_manifest))`

用途：

- build-service `POST /v1/builds` 请求中的 `manifestHash`
- `JSBundle.spec.manifest_hash`
- `JSBundle` label `frontend-forge.io/manifest-hash`
- `FI.status.observed_manifest_hash`（在成功态由 controller 从 bundle 回写）

### 4.3 Label 值格式（实现细节）

Kubernetes label value 不能包含 `:`，因此代码会把 `sha256:abcd...` 转成 `abcd...` 写入 label。

- `spec_hash` / `manifest_hash` 原始值仍保留 `sha256:` 前缀（用于 status/spec/env）
- label 中存去前缀版本

## 5. 防旧任务覆盖（当前实现）

当前实现的防 stale 策略分两层：

### 5.1 Runner 启动前校验（spec hash）

Runner 启动后先读取 FI，重新计算 `serializable_hash(fi.spec)`：

- 若与 Job 注入的 `SPEC_HASH` 不一致：说明 Job 已过期，直接退出（不构建）
- 一致：继续渲染 Manifest 并构建

### 5.2 Runner 写入前 stale-check（status hash）

Runner 在写 `JSBundle` 前轮询 FI 状态：

- 优先读取 `FI.status.observed_spec_hash`
- 兼容 fallback 到 `FI.status.observed_manifest_hash`（老状态兼容）

判定：

- 等于 `SPEC_HASH`：允许写入
- 不等于：判定 stale，退出且不写产物
- 未设置：宽限期内重试，超时失败

### 5.3 Controller 二次校验（JSBundle spec-hash label）

Controller 在处理 `Job Succeeded` 时不会仅凭 Job 成功就标记 FI 成功，而是检查：

- `JSBundle` 存在
- `JSBundle.metadata.labels["frontend-forge.io/spec-hash"]` 匹配当前 `spec_hash`

匹配才回写 `Succeeded`。

## 6. Controller Reconcile 流程（当前代码）

### 6.1 输入与 watch

- Watch `FrontendIntegration`
- Watch owned `Job`
- Watch owned `JSBundle`

### 6.2 Reconcile 主流程

1. 读取 FI
2. 若 FI 正在删除：`await_change`
3. 若 `spec.enabled=false`：
   - 回写 `phase=Pending`
   - `message=Disabled`
   - 保留已有 hash/bundle 引用（尽量不破坏当前状态）
4. 计算 `spec_hash = sha256(canonical_json(FI.spec))`
5. 计算期望 `JSBundle` 名称（当前实现：`fi-<fi-name>`）
6. 判断是否需要新构建：
   - `observed_spec_hash`（兼容 fallback `observed_manifest_hash`）与 `spec_hash` 不同
   - 首次无状态
   - 当前 `phase=Failed`（允许重试）
7. 若需要构建：
   - 按 `fi-name + spec-hash` 查找现有 Job（复用）
   - 若不存在则创建 Job（注入 `SPEC_HASH`）
   - 回写 `FI.status.phase=Building`、`observed_spec_hash=spec_hash`
8. 若不需要构建：
   - 观察 Job 状态（Pending/Running/Succeeded/Failed）
   - `Succeeded` 时读取 `JSBundle` 并校验 `spec-hash` label
   - 成功则回写：
     - `phase=Succeeded`
     - `observed_spec_hash`
     - `observed_manifest_hash = JSBundle.spec.manifest_hash`
     - `bundle_ref`

## 7. Runner 流程（当前代码）

### 7.1 Job 输入（环境变量）

当前 Job env（核心字段）：

- `FI_NAMESPACE`
- `FI_NAME`
- `SPEC_HASH`
- `JSBUNDLE_NAME`
- `BUILD_SERVICE_BASE_URL`
- `BUILD_SERVICE_TIMEOUT_SECONDS`
- `STALE_CHECK_GRACE_SECONDS`

兼容行为：

- Runner 读取 `SPEC_HASH`
- 若不存在，回退读取旧变量 `MANIFEST_HASH`（兼容旧 Job 模板）

### 7.2 构建流程

1. 读取 FI（`FI_NAMESPACE/FI_NAME`）
2. 计算 `FI.spec` hash，校验是否等于 `SPEC_HASH`
3. 按 `spec.builder.engineVersion` 将 FI 转换成 Manifest
4. 计算 `manifest_hash`
5. 调用 build-service：
   - `POST /v1/builds`（传 `manifestHash + manifest`）
   - 轮询构建状态
   - 拉取产物文件列表
6. 执行 stale-check（对齐 `FI.status.observed_spec_hash`）
7. 创建/更新固定名 `JSBundle`
8. 退出，由 Controller 回写 FI 状态

### 7.3 JSBundle 写入内容（当前实现）

`JSBundle.spec`：

- `manifest_hash`
- `files[]`

`JSBundle.metadata.labels`（核心）：

- `frontend-forge.io/managed-by`
- `frontend-forge.io/fi-name`
- `frontend-forge.io/spec-hash`
- `frontend-forge.io/manifest-hash`

`JSBundle.metadata.annotations`：

- `frontend-forge.io/build-job`（runner 从 `HOSTNAME` 推导）

## 8. FI -> Manifest 转换（Runner 多版本支持）

### 8.1 版本分发入口

当前 runner 在 `crates/runner/src/manifest.rs` 中做版本分发：

- 默认版本：`v1`
- 支持别名：`v1` / `v1alpha1` / `1` / `1.0`
- 其他值：返回 `UnsupportedEngineVersion`

### 8.2 v1 实现位置

- `render_v1_manifest` 已拆分到 `crates/runner/src/manifest/v1.rs`

这样后续新增 `v2` 时可按相同结构继续扩展，而不影响 controller 的幂等逻辑。

## 9. build-service HTTP 契约（当前实现使用）

### 9.1 创建构建

`POST /v1/builds`

```json
{
  "manifestHash": "sha256:...",
  "manifest": "{...json string...}",
  "context": {
    "namespace": "default",
    "frontendIntegration": "demo"
  }
}
```

### 9.2 查询状态

`GET /v1/builds/{id}`

```json
{
  "buildId": "bld_123",
  "status": "PENDING|RUNNING|SUCCEEDED|FAILED",
  "message": "optional"
}
```

### 9.3 获取产物文件

`GET /v1/builds/{id}/files`

```json
{
  "buildId": "bld_123",
  "files": [
    {
      "path": "index.js",
      "encoding": "base64",
      "content": "...",
      "sha256": "...",
      "size": 123,
      "contentType": "application/javascript"
    }
  ]
}
```

## 10. Rust 工程结构（当前实现）

- `crates/common`
  - canonical JSON
  - hash 计算（`manifest_hash` / `serializable_hash`）
  - label/annotation 常量（含 `spec-hash`）
  - 名称生成（Job/Bundle）
- `crates/api`
  - `FrontendIntegration` / `JSBundle` CRD 类型
  - `ManifestRenderError`（供 runner 复用）
  - 不再包含 Manifest 渲染实现
- `crates/controller`
  - FI Controller
  - 基于 `spec_hash` 的 Job 编排与状态收敛
- `crates/runner`
  - build-service 客户端
  - FI -> Manifest 版本分发（`manifest.rs`）
  - v1 渲染实现（`manifest/v1.rs`）
  - stale-check + JSBundle upsert

## 11. 当前实现边界（MVP）

- 无 Manifest `ConfigMap` / `Secret`
- `JSBundle` 名称当前固定为 `fi-<fi-name>`（未实现自定义 bundleName）
- `FI.status.conditions` 结构已定义，但当前 controller 主要使用 `phase/message`
- 大产物分片/外部对象存储未实现

## 12. 后续建议

1. 增加 `v2` engine renderer，并补版本迁移策略文档
2. 为 `spec_hash` / `manifest_hash` 增加集成测试（旧 Job 晚完成覆盖保护）
3. 若接入外部 `JSBundle` CRD，增加 schema 适配层
4. 补充 metrics / Event 上报与可观测性
