# CYRENE Runtime Identity

This internal crate resolves native service UID/GID selectors to numeric values before a process configures Unix-domain peer credential checks. Numeric selectors remain supported for managed drop-ins; account and group names let signed systemd units follow the host's installed identities.

The crate does not perform peer authorization. Callers pass the resolved numeric IDs to the existing exact `SO_PEERCRED` checks, which remain fail-closed.

## 中文

这个内部 crate 会在进程配置 Unix 域套接字对端凭据检查前，将原生服务 UID/GID 选择器解析为数值。它保留 managed drop-in 使用的数字选择器，并允许签名 systemd unit 按宿主机实际账户和组名称工作。

本 crate 不负责对端授权。调用方仍将解析后的数值交给现有精确 `SO_PEERCRED` 检查；原有失败关闭行为保持不变。
