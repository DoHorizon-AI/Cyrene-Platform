# Cyrene Central Repository Lifecycle Matrix

This matrix provides the canonical classification, authorities, and lifecycle roles for all repositories in the Cyrene workspace.

---

| Repository | Lifecycle Class | Visibility | Build Unit | Deployable | Product | Multi-Repo Req | CI Authority | Release Role | Release Authority | Deployment Authority | Distribution Profiles |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **`Cyrene-Platform`** | `PUBLIC_FOUNDATION` | Public | Yes | Daemon/Agent | No (Substrate) | No (Trust Base) | `github` | `COMPONENT_RELEASE` | `github_releases` / `pypi` / `ghcr` | `community` / `dohorizon_azure` | `core`, `training`, `serving`, `gateway`, `full` |
| **`Cyrene-Plugins`** | `PUBLIC_COMPONENT_COLLECTION` | Public | Independent | Sidecars/Workers | No (Capabilities)| Yes (Platform schemas) | `github` | `MULTI_COMPONENT_COLLECTION` | `github_releases` / `pypi` / `ghcr` | `community` | `training`, `serving`, `gateway`, `full` |
| **`Cyrene-Yield`** | `PUBLIC_PRODUCT` | Public | Yes | Yes (Daemon) | Yes (Training) | Yes (Platform + Plugins)| `github` | `COMPONENT_RELEASE` | `ghcr` / `github_releases` | `community` / `dohorizon_azure` | `training`, `full` |
| **`cyrene-reactor`** | `PUBLIC_PRODUCT` | Public | Yes | Yes (Daemon) | Yes (Serving) | Yes (Platform + Plugins)| `github` | `COMPONENT_RELEASE` | `ghcr` / `github_releases` | `community` / `dohorizon_azure` | `serving`, `full` |
| **`cyrene-exchange`**| `PUBLIC_PRODUCT` | Public | Yes | Yes (Router) | Yes (Gateway) | Yes (Platform + Plugins)| `github` | `COMPONENT_RELEASE` | `ghcr` / `github_releases` | `community` / `dohorizon_azure` | `gateway`, `full` |
| **`cyrene-astrbot-rev`**| `PUBLIC_PRODUCT` | Public | Yes | Yes (Bot) | Yes (Agent Hub)| Yes (Platform contracts)| `github` | `COMPONENT_RELEASE` | `github_releases` / `ghcr` | `community` / `dohorizon_azure` | `agent_ext`, `full` |
| **`cyrene-catalyst`**| `PUBLIC_PRODUCT` | Public | Yes | Yes | Yes (Dialogue) | Yes (Platform) | `github` | `SOURCE_ONLY` | `github_releases` | `dohorizon_azure` | `dialogue_ext` |
| **`cyrene-echo`** | `PUBLIC_PRODUCT` | Public | Yes | Yes | Yes (Audio) | Yes (Platform) | `github` | `SOURCE_ONLY` | `github_releases` | `dohorizon_azure` | `audio_ext` |
| **`cyrene-navigator`**| `PUBLIC_PRODUCT` | Public | Yes | Yes (Desktop) | Yes (Client GUI) | Yes (Platform API) | `github` | `COMPONENT_RELEASE` | `github_releases` | `community` / `dohorizon_azure` | `client_desktop` |
| **`cyrene-dh-system-internal`**| `PRIVATE_INTERNAL`| Private | Yes | Yes (WeCom Hub) | Internal Tool | Yes (Private infra) | `azure_devops` | `INTERNAL_DEPLOYMENT_ONLY` | `azure_artifacts` | `dohorizon_azure` | None (Private only) |
