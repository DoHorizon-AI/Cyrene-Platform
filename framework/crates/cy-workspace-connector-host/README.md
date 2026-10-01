# cy-workspace-connector-host

This independently versioned Cargo package owns the outbound Workspace
Connector process and its container build. It consumes the shared
`cy-workspace-fabric` library. The process reads service-owned private files,
constructs its device identity from the provisioned certificate, and connects
outbound to Relay; it does not issue credentials or own Directory authorization.

Build the package or image independently from the Platform workspace root:

```bash
cargo build --locked --release -p cy-workspace-connector-host
docker build -f framework/crates/cy-workspace-connector-host/Dockerfile .
```

The image workflow is build-only and does not publish or deploy. Product
endpoint configuration remains private and scoped. Product operation routing is
still implemented in `cy-workspace-fabric`; its safe extraction boundary and
next contract are recorded in
[`../cy-workspace-fabric/PRODUCT_ADAPTER_COMPONENT_BOUNDARY.md`](../cy-workspace-fabric/PRODUCT_ADAPTER_COMPONENT_BOUNDARY.md).
Connector file and identity requirements are in
[`../cy-workspace-fabric/WORKSPACE_CONNECTOR_HOST.md`](../cy-workspace-fabric/WORKSPACE_CONNECTOR_HOST.md).

本 Cargo package 独立拥有出站 Workspace Connector 进程与镜像构建，并消费共享的
`cy-workspace-fabric` library。进程只读取服务专属私有文件、根据预配置证书构造设备身份并
出站连接 Relay；不签发凭据，也不拥有 Directory 授权。

现有 image workflow 仅构建，不发布或部署。Product endpoint 配置仍是私有且精确 scope；
Product operation 路由仍在 `cy-workspace-fabric`，后续安全拆分接口见上方边界文档。
