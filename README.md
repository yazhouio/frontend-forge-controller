# Frontend Forge Controller

Frontend Forge Controller is a Kubernetes controller that manages frontend integrations.

## Development

- Generate the `FrontendIntegration` CRD from the Rust structs with `cargo xtask gen-crd`.
- Install git hooks with `lefthook install`. The hooks regenerate the CRD on `pre-commit` and verify it is up to date on `pre-push`.
