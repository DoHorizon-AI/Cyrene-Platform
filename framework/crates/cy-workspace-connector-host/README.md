# cy-workspace-connector-host

This independently versioned Cargo package owns the outbound Workspace
Connector process and its container build. It uses the control-plane boundary,
the small Product contract crate, and the generic Product adapter directly.
The process reads server-owned private files, constructs its device identity
from the provisioned certificate, and connects outbound to Relay; it does not
issue credentials or own Directory authorization.

Build the package or image independently from the Platform workspace root:

```bash
cargo build --locked --release -p cy-workspace-connector-host
docker build -f framework/crates/cy-workspace-connector-host/Dockerfile .
```

The image workflow is build-only and does not publish or deploy. Product
endpoint configuration remains private and scoped. At runtime, the release
bundle mounted at `/mnt/cyrene-input/product-contracts` is copied by the root
entrypoint into the root-owned, read-only
`/run/cyrene-workspace-product-contracts` directory before the process drops to
UID 10001. Native installers may set the Connector process environment variable
`CYRENE_WORKSPACE_CONNECTOR_PRODUCT_CONTRACT_ROOT_V2` to an absolute,
versioned contract directory such as
`<release_dir>/share/cyrene/product-contracts`; when unset, the binary uses
`/run/cyrene-workspace-product-contracts`. The variable selects only the file
directory. The binary embeds the reviewed
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`
pins and rejects a bundle or policy whose digest does not match that lock,
regardless of the selected directory.
Product v2 calls resolve method, path, and schemas from the pinned owner
catalogs; Platform authorization remains separate and defaults to deny.

The image workflow does not build or deploy the release bundle. Product
adapter extraction rules are recorded in
[`../cy-workspace-fabric/PRODUCT_ADAPTER_COMPONENT_BOUNDARY.md`](../cy-workspace-fabric/PRODUCT_ADAPTER_COMPONENT_BOUNDARY.md).
Connector file and identity requirements are in
[`../cy-workspace-fabric/WORKSPACE_CONNECTOR_HOST.md`](../cy-workspace-fabric/WORKSPACE_CONNECTOR_HOST.md).

本 Cargo package 独立拥有出站 Workspace Connector 进程与镜像构建，并消费共享的
control-plane、Product contracts 与通用 Product adapter crates；现有 Relay session serving
glue 仍来自 `cy-workspace-fabric`。进程只读取服务专属私有文件、根据预配置证书构造设备身份
并出站连接 Relay；不签发凭据，也不拥有 Directory 授权。

现有 image workflow 仅构建，不发布或部署。Product endpoint 配置仍是私有且精确 scope；
Product v2 路由和 schema 来自 lock 固定的 owner release catalogs；Platform policy 与
response scope validation 仍由 control plane 执行。Native installer 可通过
`CYRENE_WORKSPACE_CONNECTOR_PRODUCT_CONTRACT_ROOT_V2` 指向 versioned Product contract
目录；未设置时沿用 Docker 的 `/run/cyrene-workspace-product-contracts`。此变量只选择
文件目录，不能替换编译时 lock 或 policy pin。组件边界和运行输入见上方边界文档。
