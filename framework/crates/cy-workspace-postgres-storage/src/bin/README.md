# Workspace storage administration tools

This directory contains bounded command-line maintenance entrypoints for PostgreSQL storage adapters.

| File | Responsibility |
| --- | --- |
| `cy-workspace-device-ca-admin.rs` | Applies the restricted device CA migration or checks the configured signer and current signed CRL. |

Run `cy-workspace-device-ca-admin migrate` only with the separately provisioned migration URL. Run `check` with the restricted runtime URL and owner-protected CA key/certificate paths. Neither command prints credentials or key material.
