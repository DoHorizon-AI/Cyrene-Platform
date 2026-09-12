# Security boundary

This repository contains a generic execution substrate. Please do not report
the resource-governance guarantees below as a claim of complete hostile-code
containment.

## Reporting

For a suspected vulnerability, provide a minimal reproduction, affected
component and version, operating-system assumptions, and whether the process
was running with delegated cgroup or root privileges. Do not include secrets or
real tenant data in an issue.

## Scope and current guarantees

Platform Core owns principal/lease/fence authority, lifecycle decisions,
bounded local IPC, and the cleanup decision. The privileged `cyrene-sandboxd`
service owns its delegated cgroup subtree, cgroup resource controls, device
policy enforcement where enabled, pidfd-backed tracking where available, and
descendant cleanup using `cgroup.kill`.

Local UDS services use filesystem permissions and, where configured, Linux
`SO_PEERCRED` UID/GID checks before accepting a request. The Linux system
Adapter currently permits an explicit filesystem-permission-only deployment;
production deployments should configure both peer identities. Relay traffic
uses the documented mTLS configuration.

## Important non-guarantees

The current sandbox implementation does not establish user, mount, network, or
PID namespace isolation; it does not install seccomp filters or drop Linux
capabilities; and it does not provide complete host-filesystem or syscall
containment. It must not be marketed as a security sandbox for hostile,
arbitrary, mutually untrusted workloads or as a complete multi-tenant boundary.
It also does not justify an absolute claim such as “zero orphan processes under
all conditions”. The accurate claim is cgroup-scoped lifecycle management with
bounded reaping, pidfd tracking when available, and cgroup-scoped cleanup.

See [the threat model](docs/security/threat-model.md) and the runtime
operations guide for deployment prerequisites and evidence boundaries.
