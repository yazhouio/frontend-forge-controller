#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEBUG_DIR="${DEBUG_DIR:-$ROOT_DIR/.codex-debug}"
TLS_DIR="$DEBUG_DIR/webhook-tls"
LOCAL_KUBECONFIG="$DEBUG_DIR/remote-kind-kubeconfig.yaml"
CONTROLLER_LOG="$DEBUG_DIR/controller.log"
CLOUDFLARED_LOG="$DEBUG_DIR/cloudflared.log"
CONTROLLER_PID_FILE="$DEBUG_DIR/controller.pid"
CLOUDFLARED_PID_FILE="$DEBUG_DIR/cloudflared.pid"
TUNNEL_URL_FILE="$DEBUG_DIR/cloudflared-url.txt"
WEBHOOK_BACKUP_FILE="$DEBUG_DIR/frontend-forge-controller-validating-webhook.backup.yaml"

REMOTE_SSH_TARGET="${REMOTE_SSH_TARGET:-}"
REMOTE_CONTEXT="${REMOTE_CONTEXT:-}"
REMOTE_NAMESPACE="${REMOTE_NAMESPACE:-extension-frontend-forge}"
REMOTE_CONTROLLER_DEPLOYMENT="${REMOTE_CONTROLLER_DEPLOYMENT:-frontend-forge-controller}"
REMOTE_WEBHOOK_CONFIG="${REMOTE_WEBHOOK_CONFIG:-frontend-forge-controller-validating-webhook}"
REMOTE_WEBHOOK_BACKUP_PATH="${REMOTE_WEBHOOK_BACKUP_PATH:-/root/frontend-forge-controller-validating-webhook.backup.yaml}"
REMOTE_FAILURE_POLICY="${REMOTE_FAILURE_POLICY:-Ignore}"

LOCAL_APISERVER_URL="${LOCAL_APISERVER_URL:-https://127.0.0.1:44321}"
LOCAL_WEBHOOK_BIND_ADDR="${LOCAL_WEBHOOK_BIND_ADDR:-127.0.0.1:9443}"
LOCAL_CLOUDFLARED_METRICS="${LOCAL_CLOUDFLARED_METRICS:-127.0.0.1:20241}"
SELF_SIGNED_CERT_CN="${SELF_SIGNED_CERT_CN:-localhost}"

WORK_NAMESPACE="${WORK_NAMESPACE:-$REMOTE_NAMESPACE}"
RUNNER_IMAGE="${RUNNER_IMAGE:-}"
RUNNER_SERVICE_ACCOUNT="${RUNNER_SERVICE_ACCOUNT:-}"
BUILD_SERVICE_BASE_URL="${BUILD_SERVICE_BASE_URL:-}"
JSBUNDLE_CONFIGMAP_NAMESPACE="${JSBUNDLE_CONFIGMAP_NAMESPACE:-}"
JSBUNDLE_CONFIG_KEY="${JSBUNDLE_CONFIG_KEY:-}"
BUILD_SERVICE_TIMEOUT_SECONDS="${BUILD_SERVICE_TIMEOUT_SECONDS:-}"
STALE_CHECK_GRACE_SECONDS="${STALE_CHECK_GRACE_SECONDS:-}"
RECONCILE_REQUEUE_SECONDS="${RECONCILE_REQUEUE_SECONDS:-}"
JOB_ACTIVE_DEADLINE_SECONDS="${JOB_ACTIVE_DEADLINE_SECONDS:-}"
JOB_TTL_SECONDS_AFTER_FINISHED="${JOB_TTL_SECONDS_AFTER_FINISHED:-}"
RUST_LOG_VALUE="${RUST_LOG_VALUE:-info,frontend_forge_controller=debug}"

usage() {
  cat <<'EOF'
Usage:
  scripts/dev-webhook.sh start
  scripts/dev-webhook.sh stop
  scripts/dev-webhook.sh restart
  scripts/dev-webhook.sh status
  scripts/dev-webhook.sh logs controller
  scripts/dev-webhook.sh logs cloudflared

Required environment:
  REMOTE_SSH_TARGET        Example: root@172.31.19.2

Optional environment:
  REMOTE_CONTEXT           Remote kubectl context to use
  REMOTE_NAMESPACE         Default: extension-frontend-forge
  REMOTE_CONTROLLER_DEPLOYMENT
  REMOTE_WEBHOOK_CONFIG
  REMOTE_WEBHOOK_BACKUP_PATH
  REMOTE_FAILURE_POLICY    Default: Ignore
  LOCAL_APISERVER_URL      Default: https://127.0.0.1:44321
  LOCAL_WEBHOOK_BIND_ADDR  Default: 127.0.0.1:9443
  LOCAL_CLOUDFLARED_METRICS
  DEBUG_DIR

Notes:
  1. Keep your SSH tunnel to the remote apiserver open before running start.
  2. start will:
     - fetch remote kubeconfig and rewrite it to LOCAL_APISERVER_URL
     - start a local controller/webhook process
     - start a Cloudflare quick tunnel
     - patch the remote ValidatingWebhookConfiguration to clientConfig.url
  3. stop will restore the webhook configuration from the saved backup.
EOF
}

log() {
  printf '[dev-webhook] %s\n' "$*" >&2
}

die() {
  log "$*"
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

pid_is_running() {
  local pid_file="$1"
  [[ -f "$pid_file" ]] || return 1
  local pid
  pid="$(cat "$pid_file")"
  [[ -n "$pid" ]] || return 1
  kill -0 "$pid" 2>/dev/null
}

kill_pidfile() {
  local pid_file="$1"
  if pid_is_running "$pid_file"; then
    local pid
    pid="$(cat "$pid_file")"
    kill "$pid"
    for _ in $(seq 1 20); do
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 1
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill -9 "$pid"
    fi
  fi
  rm -f "$pid_file"
}

stop_local_processes() {
  kill_pidfile "$CLOUDFLARED_PID_FILE"
  kill_pidfile "$CONTROLLER_PID_FILE"
  rm -f "$TUNNEL_URL_FILE"
}

remote_kubectl() {
  local ssh_args=(-o BatchMode=yes "$REMOTE_SSH_TARGET")
  local kubectl_args=(kubectl)
  if [[ -n "$REMOTE_CONTEXT" ]]; then
    kubectl_args+=(--context "$REMOTE_CONTEXT")
  fi
  ssh "${ssh_args[@]}" "${kubectl_args[@]}" "$@"
}

ensure_prereqs() {
  require_cmd ssh
  require_cmd kubectl
  require_cmd cargo
  require_cmd cloudflared
  require_cmd openssl
  require_cmd curl
  require_cmd perl
  require_cmd lsof
  [[ -n "$REMOTE_SSH_TARGET" ]] || die "REMOTE_SSH_TARGET is required"
}

ensure_local_apiserver_tunnel() {
  curl -sk "${LOCAL_APISERVER_URL}/healthz" >/dev/null \
    || die "cannot reach LOCAL_APISERVER_URL=${LOCAL_APISERVER_URL}; keep your ssh -L tunnel open"
}

remote_deploy_env() {
  local name="$1"
  local remote_args=("$REMOTE_NAMESPACE" "$REMOTE_CONTROLLER_DEPLOYMENT" "$name")
  if [[ -n "$REMOTE_CONTEXT" ]]; then
    remote_args+=("$REMOTE_CONTEXT")
  fi
  ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" /bin/bash -s -- "${remote_args[@]}" <<'EOF'
namespace="$1"
deployment="$2"
env_name="$3"
context="${4:-}"
kubectl_args=()
if [[ -n "$context" ]]; then
  kubectl_args=(--context "$context")
fi
kubectl "${kubectl_args[@]}" -n "$namespace" get deploy "$deployment" \
  -o "jsonpath={.spec.template.spec.containers[0].env[?(@.name=='$env_name')].value}"
EOF
}

load_runtime_env_defaults() {
  [[ -n "$RUNNER_IMAGE" ]] || RUNNER_IMAGE="$(remote_deploy_env RUNNER_IMAGE)"
  [[ -n "$RUNNER_SERVICE_ACCOUNT" ]] || RUNNER_SERVICE_ACCOUNT="$(remote_deploy_env RUNNER_SERVICE_ACCOUNT)"
  [[ -n "$BUILD_SERVICE_BASE_URL" ]] || BUILD_SERVICE_BASE_URL="$(remote_deploy_env BUILD_SERVICE_BASE_URL)"
  [[ -n "$JSBUNDLE_CONFIGMAP_NAMESPACE" ]] || JSBUNDLE_CONFIGMAP_NAMESPACE="$(remote_deploy_env JSBUNDLE_CONFIGMAP_NAMESPACE)"
  [[ -n "$JSBUNDLE_CONFIG_KEY" ]] || JSBUNDLE_CONFIG_KEY="$(remote_deploy_env JSBUNDLE_CONFIG_KEY)"
  [[ -n "$BUILD_SERVICE_TIMEOUT_SECONDS" ]] || BUILD_SERVICE_TIMEOUT_SECONDS="$(remote_deploy_env BUILD_SERVICE_TIMEOUT_SECONDS)"
  [[ -n "$STALE_CHECK_GRACE_SECONDS" ]] || STALE_CHECK_GRACE_SECONDS="$(remote_deploy_env STALE_CHECK_GRACE_SECONDS)"
  [[ -n "$RECONCILE_REQUEUE_SECONDS" ]] || RECONCILE_REQUEUE_SECONDS="$(remote_deploy_env RECONCILE_REQUEUE_SECONDS)"
  [[ -n "$JOB_ACTIVE_DEADLINE_SECONDS" ]] || JOB_ACTIVE_DEADLINE_SECONDS="$(remote_deploy_env JOB_ACTIVE_DEADLINE_SECONDS)"
  [[ -n "$JOB_TTL_SECONDS_AFTER_FINISHED" ]] || JOB_TTL_SECONDS_AFTER_FINISHED="$(remote_deploy_env JOB_TTL_SECONDS_AFTER_FINISHED)"

  [[ -n "$RUNNER_IMAGE" ]] || die "RUNNER_IMAGE is empty; set it explicitly or make sure the remote deployment defines it"
  [[ -n "$BUILD_SERVICE_BASE_URL" ]] || BUILD_SERVICE_BASE_URL="http://frontend-forge.${REMOTE_NAMESPACE}.svc"
  [[ -n "$JSBUNDLE_CONFIGMAP_NAMESPACE" ]] || JSBUNDLE_CONFIGMAP_NAMESPACE="$REMOTE_NAMESPACE"
  [[ -n "$JSBUNDLE_CONFIG_KEY" ]] || JSBUNDLE_CONFIG_KEY="index.js"
  [[ -n "$BUILD_SERVICE_TIMEOUT_SECONDS" ]] || BUILD_SERVICE_TIMEOUT_SECONDS="600"
  [[ -n "$STALE_CHECK_GRACE_SECONDS" ]] || STALE_CHECK_GRACE_SECONDS="30"
  [[ -n "$RECONCILE_REQUEUE_SECONDS" ]] || RECONCILE_REQUEUE_SECONDS="5"
  [[ -n "$JOB_ACTIVE_DEADLINE_SECONDS" ]] || JOB_ACTIVE_DEADLINE_SECONDS="300"
  [[ -n "$JOB_TTL_SECONDS_AFTER_FINISHED" ]] || JOB_TTL_SECONDS_AFTER_FINISHED="3600"
}

ensure_debug_dir() {
  mkdir -p "$DEBUG_DIR" "$TLS_DIR"
}

ensure_remote_backup() {
  if ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" "test -f '$REMOTE_WEBHOOK_BACKUP_PATH'"; then
    return
  fi

  log "saving remote webhook backup to $REMOTE_WEBHOOK_BACKUP_PATH"
  remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" \
    -o yaml --show-managed-fields=false \
    | ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" "cat > '$REMOTE_WEBHOOK_BACKUP_PATH'"
}

ensure_local_backup() {
  if [[ -f "$WEBHOOK_BACKUP_FILE" ]]; then
    return
  fi

  if ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" "test -f '$REMOTE_WEBHOOK_BACKUP_PATH'"; then
    log "copying remote webhook backup to $WEBHOOK_BACKUP_FILE"
    ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" "cat '$REMOTE_WEBHOOK_BACKUP_PATH'" > "$WEBHOOK_BACKUP_FILE"
    return
  fi

  log "capturing current webhook configuration to $WEBHOOK_BACKUP_FILE"
  remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" \
    -o yaml --show-managed-fields=false > "$WEBHOOK_BACKUP_FILE"
}

write_local_kubeconfig() {
  log "fetching remote kubeconfig"
  remote_kubectl config view --raw --minify > "$LOCAL_KUBECONFIG"
  LOCAL_APISERVER_URL="$LOCAL_APISERVER_URL" \
    perl -0pi -e 's#^(\s*server: ).*$#$1$ENV{LOCAL_APISERVER_URL}#m' "$LOCAL_KUBECONFIG"
  grep -q "$LOCAL_APISERVER_URL" "$LOCAL_KUBECONFIG" \
    || die "failed to rewrite kubeconfig server address to $LOCAL_APISERVER_URL"
}

ensure_tls_assets() {
  if [[ -s "$TLS_DIR/tls.crt" && -s "$TLS_DIR/tls.key" ]]; then
    return
  fi

  log "generating local self-signed TLS certificate"
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TLS_DIR/tls.key" \
    -out "$TLS_DIR/tls.crt" \
    -subj "/CN=${SELF_SIGNED_CERT_CN}" \
    -days 1 >/dev/null 2>&1
}

assert_port_free() {
  local addr="$1"
  local port="${addr##*:}"
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    die "port $addr is already in use; stop the existing process first"
  fi
}

spawn_detached() {
  local log_file="$1"
  shift

  perl -e '
    use strict;
    use warnings;
    use POSIX qw(setsid);

    my ($log_file, @cmd) = @ARGV;
    defined(my $pid = fork) or die "fork failed: $!";
    if ($pid) {
      print "$pid\n";
      exit 0;
    }

    setsid() or die "setsid failed: $!";
    open STDIN, "<", "/dev/null" or die "open stdin failed: $!";
    open STDOUT, ">>", $log_file or die "open stdout failed: $!";
    open STDERR, ">&STDOUT" or die "redirect stderr failed: $!";
    exec @cmd or die "exec failed: $!";
  ' "$log_file" "$@"
}

build_controller() {
  log "building frontend-forge-controller"
  (cd "$ROOT_DIR" && cargo build -p frontend-forge-controller >/dev/null)
}

wait_for_local_webhook() {
  for _ in $(seq 1 30); do
    if curl -sk "https://${LOCAL_WEBHOOK_BIND_ADDR}/healthz" >/dev/null; then
      return
    fi

    if ! pid_is_running "$CONTROLLER_PID_FILE"; then
      tail -n 50 "$CONTROLLER_LOG" >&2 || true
      die "local controller exited before /healthz became ready"
    fi
    sleep 1
  done

  tail -n 50 "$CONTROLLER_LOG" >&2 || true
  die "timed out waiting for local webhook health check"
}

start_controller() {
  if pid_is_running "$CONTROLLER_PID_FILE"; then
    die "local controller is already running with pid $(cat "$CONTROLLER_PID_FILE")"
  fi

  : > "$CONTROLLER_LOG"
  (
    cd "$ROOT_DIR"
    spawn_detached "$CONTROLLER_LOG" \
      env \
        KUBECONFIG="$LOCAL_KUBECONFIG" \
        WORK_NAMESPACE="$WORK_NAMESPACE" \
        RUNNER_IMAGE="$RUNNER_IMAGE" \
        RUNNER_SERVICE_ACCOUNT="$RUNNER_SERVICE_ACCOUNT" \
        BUILD_SERVICE_BASE_URL="$BUILD_SERVICE_BASE_URL" \
        JSBUNDLE_CONFIGMAP_NAMESPACE="$JSBUNDLE_CONFIGMAP_NAMESPACE" \
        JSBUNDLE_CONFIG_KEY="$JSBUNDLE_CONFIG_KEY" \
        BUILD_SERVICE_TIMEOUT_SECONDS="$BUILD_SERVICE_TIMEOUT_SECONDS" \
        STALE_CHECK_GRACE_SECONDS="$STALE_CHECK_GRACE_SECONDS" \
        RECONCILE_REQUEUE_SECONDS="$RECONCILE_REQUEUE_SECONDS" \
        JOB_ACTIVE_DEADLINE_SECONDS="$JOB_ACTIVE_DEADLINE_SECONDS" \
        JOB_TTL_SECONDS_AFTER_FINISHED="$JOB_TTL_SECONDS_AFTER_FINISHED" \
        WEBHOOK_ENABLED=true \
        WEBHOOK_BIND_ADDR="$LOCAL_WEBHOOK_BIND_ADDR" \
        WEBHOOK_CERT_PATH="$TLS_DIR/tls.crt" \
        WEBHOOK_KEY_PATH="$TLS_DIR/tls.key" \
        RUST_LOG="$RUST_LOG_VALUE" \
        "$ROOT_DIR/target/debug/frontend-forge-controller" \
      > "$CONTROLLER_PID_FILE"
  )
  wait_for_local_webhook
}

extract_tunnel_url() {
  if [[ -f "$TUNNEL_URL_FILE" ]]; then
    cat "$TUNNEL_URL_FILE"
    return
  fi
  grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$CLOUDFLARED_LOG" 2>/dev/null | tail -n 1 || true
}

wait_for_tunnel_url() {
  local url=""
  for _ in $(seq 1 30); do
    url="$(extract_tunnel_url)"
    if [[ -n "$url" ]]; then
      printf '%s\n' "$url" > "$TUNNEL_URL_FILE"
      printf '%s\n' "$url"
      return
    fi
    if ! pid_is_running "$CLOUDFLARED_PID_FILE"; then
      tail -n 50 "$CLOUDFLARED_LOG" >&2 || true
      die "cloudflared exited before publishing a tunnel URL"
    fi
    sleep 1
  done
  tail -n 50 "$CLOUDFLARED_LOG" >&2 || true
  die "timed out waiting for cloudflared quick tunnel URL"
}

start_cloudflared() {
  if pid_is_running "$CLOUDFLARED_PID_FILE"; then
    die "cloudflared is already running with pid $(cat "$CLOUDFLARED_PID_FILE")"
  fi

  rm -f "$TUNNEL_URL_FILE"
  : > "$CLOUDFLARED_LOG"
  spawn_detached "$CLOUDFLARED_LOG" \
    cloudflared tunnel \
      --url "https://${LOCAL_WEBHOOK_BIND_ADDR}" \
      --no-tls-verify \
      --metrics "$LOCAL_CLOUDFLARED_METRICS" \
      --loglevel info \
    > "$CLOUDFLARED_PID_FILE"
  wait_for_tunnel_url
}

join_by() {
  local delimiter="$1"
  shift
  local first=1
  for item in "$@"; do
    if [[ $first -eq 1 ]]; then
      printf '%s' "$item"
      first=0
    else
      printf '%s%s' "$delimiter" "$item"
    fi
  done
}

patch_remote_webhook_to_url() {
  local url="$1"
  local current_url current_service current_cabundle patch
  local ops=()

  current_url="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.url}')"
  current_service="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.service.name}')"
  current_cabundle="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.caBundle}')"

  if [[ -n "$current_service" ]]; then
    ops+=('{"op":"remove","path":"/webhooks/0/clientConfig/service"}')
  fi
  if [[ -n "$current_cabundle" ]]; then
    ops+=('{"op":"remove","path":"/webhooks/0/clientConfig/caBundle"}')
  fi
  if [[ -n "$current_url" ]]; then
    ops+=("{\"op\":\"replace\",\"path\":\"/webhooks/0/clientConfig/url\",\"value\":\"${url}\"}")
  else
    ops+=("{\"op\":\"add\",\"path\":\"/webhooks/0/clientConfig/url\",\"value\":\"${url}\"}")
  fi
  ops+=("{\"op\":\"replace\",\"path\":\"/webhooks/0/failurePolicy\",\"value\":\"${REMOTE_FAILURE_POLICY}\"}")

  patch="[$(join_by , "${ops[@]}")]"
  log "patching remote webhook to ${url}"
  ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" /bin/bash -s -- "$REMOTE_WEBHOOK_CONFIG" "$REMOTE_CONTEXT" <<EOF >/dev/null
set -euo pipefail
webhook="\$1"
context="\${2:-}"
patch_file="\$(mktemp)"
trap 'rm -f "\$patch_file"' EXIT
cat > "\$patch_file" <<'PATCH'
$patch
PATCH
kubectl_args=()
if [[ -n "\$context" ]]; then
  kubectl_args=(--context "\$context")
fi
kubectl "\${kubectl_args[@]}" patch validatingwebhookconfiguration "\$webhook" --type=json -p "\$(cat "\$patch_file")"
EOF
}

restore_remote_webhook() {
  if [[ ! -f "$WEBHOOK_BACKUP_FILE" ]]; then
    log "no local backup found at $WEBHOOK_BACKUP_FILE; skipping remote restore"
    return
  fi
  log "restoring remote webhook configuration"
  local backup_failure_policy backup_service_name backup_service_namespace
  local backup_service_path backup_service_port backup_cabundle
  local current_url current_service current_cabundle patch
  local ops=()

  backup_failure_policy="$(awk '/^webhooks:$/ {in_webhooks=1; next} in_webhooks && $1 == "failurePolicy:" {print $2; exit}' "$WEBHOOK_BACKUP_FILE")"
  backup_service_name="$(awk '/^webhooks:$/ {in_webhooks=1; next} in_webhooks && $1 == "service:" {in_service=1; next} in_service && $1 == "name:" {print $2; exit}' "$WEBHOOK_BACKUP_FILE")"
  backup_service_namespace="$(awk '/^webhooks:$/ {in_webhooks=1; next} in_webhooks && $1 == "service:" {in_service=1; next} in_service && $1 == "namespace:" {print $2; exit}' "$WEBHOOK_BACKUP_FILE")"
  backup_service_path="$(awk '/^webhooks:$/ {in_webhooks=1; next} in_webhooks && $1 == "service:" {in_service=1; next} in_service && $1 == "path:" {print $2; exit}' "$WEBHOOK_BACKUP_FILE")"
  backup_service_port="$(awk '/^webhooks:$/ {in_webhooks=1; next} in_webhooks && $1 == "service:" {in_service=1; next} in_service && $1 == "port:" {print $2; exit}' "$WEBHOOK_BACKUP_FILE")"
  backup_cabundle="$(awk '/^webhooks:$/ {in_webhooks=1; next} in_webhooks && $1 == "caBundle:" {print $2; exit}' "$WEBHOOK_BACKUP_FILE")"

  [[ -n "$backup_failure_policy" ]] || die "failed to read failurePolicy from $WEBHOOK_BACKUP_FILE"
  [[ -n "$backup_service_name" ]] || die "failed to read clientConfig.service.name from $WEBHOOK_BACKUP_FILE"
  [[ -n "$backup_service_namespace" ]] || die "failed to read clientConfig.service.namespace from $WEBHOOK_BACKUP_FILE"
  [[ -n "$backup_service_path" ]] || die "failed to read clientConfig.service.path from $WEBHOOK_BACKUP_FILE"
  [[ -n "$backup_service_port" ]] || die "failed to read clientConfig.service.port from $WEBHOOK_BACKUP_FILE"
  [[ -n "$backup_cabundle" ]] || die "failed to read clientConfig.caBundle from $WEBHOOK_BACKUP_FILE"

  current_url="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.url}')"
  current_service="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.service.name}')"
  current_cabundle="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.caBundle}')"

  if [[ -n "$current_url" ]]; then
    ops+=('{"op":"remove","path":"/webhooks/0/clientConfig/url"}')
  fi
  if [[ -n "$current_service" ]]; then
    ops+=("{\"op\":\"replace\",\"path\":\"/webhooks/0/clientConfig/service\",\"value\":{\"name\":\"${backup_service_name}\",\"namespace\":\"${backup_service_namespace}\",\"path\":\"${backup_service_path}\",\"port\":${backup_service_port}}}")
  else
    ops+=("{\"op\":\"add\",\"path\":\"/webhooks/0/clientConfig/service\",\"value\":{\"name\":\"${backup_service_name}\",\"namespace\":\"${backup_service_namespace}\",\"path\":\"${backup_service_path}\",\"port\":${backup_service_port}}}")
  fi
  if [[ -n "$current_cabundle" ]]; then
    ops+=("{\"op\":\"replace\",\"path\":\"/webhooks/0/clientConfig/caBundle\",\"value\":\"${backup_cabundle}\"}")
  else
    ops+=("{\"op\":\"add\",\"path\":\"/webhooks/0/clientConfig/caBundle\",\"value\":\"${backup_cabundle}\"}")
  fi
  ops+=("{\"op\":\"replace\",\"path\":\"/webhooks/0/failurePolicy\",\"value\":\"${backup_failure_policy}\"}")

  patch="[$(join_by , "${ops[@]}")]"
  ssh -o BatchMode=yes "$REMOTE_SSH_TARGET" /bin/bash -s -- "$REMOTE_WEBHOOK_CONFIG" "$REMOTE_CONTEXT" <<EOF >/dev/null
set -euo pipefail
webhook="\$1"
context="\${2:-}"
patch_file="\$(mktemp)"
trap 'rm -f "\$patch_file"' EXIT
cat > "\$patch_file" <<'PATCH'
$patch
PATCH
kubectl_args=()
if [[ -n "\$context" ]]; then
  kubectl_args=(--context "\$context")
fi
kubectl "\${kubectl_args[@]}" patch validatingwebhookconfiguration "\$webhook" --type=json -p "\$(cat "\$patch_file")"
EOF
}

smoke_test_remote_webhook() {
  local output status
  for _ in $(seq 1 30); do
    set +e
    output="$(
      cat <<'EOF' | remote_kubectl apply --dry-run=server -f - 2>&1
apiVersion: frontend-forge.kubesphere.io/v1alpha1
kind: FrontendIntegration
metadata:
  name: codex-dev-webhook-smoke
spec:
  enabled: true
  menus:
    - displayName: Demo
      key: codex-dev-page
      placement: global
      type: page
  pages:
    - key: codex-dev-page
      type: iframe
      iframe:
        src: http://example.test/1
    - key: codex-dev-page
      type: iframe
      iframe:
        src: http://example.test/2
  builder:
    engineVersion: v1
EOF
    )"
    status=$?
    set -e

    if [[ $status -ne 0 && "$output" == *"duplicate page key 'codex-dev-page'"* ]]; then
      log "remote webhook smoke test passed"
      return
    fi

    if ! pid_is_running "$CONTROLLER_PID_FILE"; then
      tail -n 50 "$CONTROLLER_LOG" >&2 || true
      die "local controller exited while waiting for the remote smoke test"
    fi
    if ! pid_is_running "$CLOUDFLARED_PID_FILE"; then
      tail -n 50 "$CLOUDFLARED_LOG" >&2 || true
      die "cloudflared exited while waiting for the remote smoke test"
    fi

    sleep 1
  done

  printf '%s\n' "$output" >&2
  die "remote webhook smoke test failed; the remote cluster did not observe the local deny response"
}

start_cmd() {
  local success=0
  trap '
    if [[ $success -ne 1 ]]; then
      log "start failed; cleaning up local processes and restoring remote webhook"
      stop_local_processes || true
      restore_remote_webhook || true
    fi
  ' RETURN

  ensure_prereqs
  ensure_local_apiserver_tunnel
  ensure_debug_dir
  ensure_remote_backup
  ensure_local_backup
  write_local_kubeconfig
  ensure_tls_assets
  load_runtime_env_defaults
  assert_port_free "$LOCAL_WEBHOOK_BIND_ADDR"
  assert_port_free "$LOCAL_CLOUDFLARED_METRICS"
  build_controller
  start_controller
  local tunnel_url
  tunnel_url="$(start_cloudflared)"
  patch_remote_webhook_to_url "${tunnel_url}/validate/frontendintegrations"
  smoke_test_remote_webhook

  success=1
  trap - RETURN
  log "ready"
  log "local controller log: $CONTROLLER_LOG"
  log "cloudflared log: $CLOUDFLARED_LOG"
  log "tunnel url: $tunnel_url"
}

stop_cmd() {
  ensure_prereqs
  stop_local_processes
  restore_remote_webhook
  log "stopped"
}

status_cmd() {
  ensure_prereqs
  local remote_url remote_service controller_state cloudflared_state tunnel_url

  remote_url="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.url}')"
  remote_service="$(remote_kubectl get validatingwebhookconfiguration "$REMOTE_WEBHOOK_CONFIG" -o 'jsonpath={.webhooks[0].clientConfig.service.name}')"
  if pid_is_running "$CONTROLLER_PID_FILE"; then
    controller_state="running pid=$(cat "$CONTROLLER_PID_FILE")"
  else
    controller_state="stopped"
  fi

  if pid_is_running "$CLOUDFLARED_PID_FILE"; then
    cloudflared_state="running pid=$(cat "$CLOUDFLARED_PID_FILE")"
    tunnel_url="$(extract_tunnel_url)"
  else
    cloudflared_state="stopped"
    tunnel_url=""
  fi

  printf 'controller: %s\n' "$controller_state"
  printf 'cloudflared: %s\n' "$cloudflared_state"
  printf 'tunnel_url: %s\n' "${tunnel_url:-<none>}"
  if [[ -n "$remote_url" ]]; then
    printf 'remote_webhook_target: %s\n' "$remote_url"
  else
    printf 'remote_webhook_target: service/%s\n' "${remote_service:-<none>}"
  fi
  printf 'controller_log: %s\n' "$CONTROLLER_LOG"
  printf 'cloudflared_log: %s\n' "$CLOUDFLARED_LOG"
}

logs_cmd() {
  local target="${1:-}"
  case "$target" in
    controller)
      tail -f "$CONTROLLER_LOG"
      ;;
    cloudflared)
      tail -f "$CLOUDFLARED_LOG"
      ;;
    *)
      die "logs target must be 'controller' or 'cloudflared'"
      ;;
  esac
}

restart_cmd() {
  stop_cmd || true
  start_cmd
}

main() {
  local cmd="${1:-}"
  case "$cmd" in
    start)
      start_cmd
      ;;
    stop)
      stop_cmd
      ;;
    restart)
      restart_cmd
      ;;
    status)
      status_cmd
      ;;
    logs)
      shift || true
      logs_cmd "${1:-}"
      ;;
    -h|--help|help|"")
      usage
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "$@"
