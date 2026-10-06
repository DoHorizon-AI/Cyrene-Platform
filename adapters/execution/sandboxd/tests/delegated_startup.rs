// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/tests/delegated_startup.rs       ║
// ║ Module: CYRENE Platform                                               ║
// ║ Role: Deterministic cgroup v2 delegated startup conformance tests.    ║
// ║                                                                      ║
// ║ 模块：CYRENE Platform                                                  ║
// ║ 职责：确定性验证 cgroup v2 委派启动边界。                              ║
// ╚══════════════════════════════════════════════════════════════════════╝
//! Tests the privileged startup handoff without changing the host cgroup tree.

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use config::{prepare_delegated_process_with, DelegatedCgroupIo, DELEGATED_PROCESS_LEAF};
use cy_kernel_api::ProviderError;

const DAEMON_PID: u32 = 37_821;

struct MockCgroupIo {
    pid: u32,
    current: PathBuf,
    groups: BTreeSet<PathBuf>,
    processes: BTreeMap<PathBuf, Vec<u32>>,
    deny_move: bool,
    leave_foreign_parent_pid: bool,
}

impl MockCgroupIo {
    fn new(parent: &Path, worker_root: &Path) -> Self {
        Self {
            pid: DAEMON_PID,
            current: parent.to_path_buf(),
            groups: [parent.to_path_buf(), worker_root.to_path_buf()]
                .into_iter()
                .collect(),
            processes: [(parent.to_path_buf(), vec![DAEMON_PID])]
                .into_iter()
                .collect(),
            deny_move: false,
            leave_foreign_parent_pid: false,
        }
    }

    fn ids(&self, group: &Path) -> Vec<u32> {
        self.processes.get(group).cloned().unwrap_or_default()
    }
}

impl DelegatedCgroupIo for MockCgroupIo {
    fn current_pid(&self) -> u32 {
        self.pid
    }

    fn current_group(&mut self) -> Result<PathBuf, ProviderError> {
        Ok(self.current.clone())
    }

    fn group_exists(&mut self, group: &Path) -> Result<bool, ProviderError> {
        Ok(self.groups.contains(group))
    }

    fn direct_processes(&mut self, group: &Path) -> Result<Vec<u32>, ProviderError> {
        Ok(self.ids(group))
    }

    fn child_groups(&mut self, group: &Path) -> Result<Vec<PathBuf>, ProviderError> {
        Ok(self
            .groups
            .iter()
            .filter(|candidate| candidate.parent() == Some(group))
            .cloned()
            .collect())
    }

    fn ensure_control_group(&mut self, parent: &Path) -> Result<PathBuf, ProviderError> {
        let group = parent.join(DELEGATED_PROCESS_LEAF);
        self.groups.insert(group.clone());
        self.processes.entry(group.clone()).or_default();
        Ok(group)
    }

    fn move_process(&mut self, pid: u32, destination: &Path) -> Result<(), ProviderError> {
        if self.deny_move {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_DELEGATION_MOVE_FAILED",
                "injected permission failure",
            ));
        }
        let source = self.current.clone();
        let source_processes = self.processes.entry(source.clone()).or_default();
        let Some(position) = source_processes.iter().position(|current| *current == pid) else {
            return Err(ProviderError::new(
                "linux-cgroup-v2",
                "CGROUP_DELEGATION_MOVE_FAILED",
                "daemon PID was not in its probed source group",
            ));
        };
        source_processes.remove(position);
        self.processes
            .entry(destination.to_path_buf())
            .or_default()
            .push(pid);
        self.current = destination.to_path_buf();
        if self.leave_foreign_parent_pid {
            self.processes.entry(source).or_default().push(44_444);
        }
        Ok(())
    }
}

fn fixture_paths() -> (PathBuf, PathBuf) {
    let parent = PathBuf::from("/sys/fs/cgroup/system.slice/cyrene-sandboxd.service");
    let worker_root = parent.join("cyrene");
    (parent, worker_root)
}

fn handoff(worker_root: &Path, io: &mut MockCgroupIo) -> Result<(), ProviderError> {
    prepare_delegated_process_with(worker_root, io, |_, _, _| Ok(()))
}

#[test]
fn handoff_moves_only_daemon_to_control_leaf_and_leaves_worker_root_sibling() {
    let (parent, worker_root) = fixture_paths();
    let control_group = parent.join(DELEGATED_PROCESS_LEAF);
    let mut io = MockCgroupIo::new(&parent, &worker_root);
    let mut controller_enable_called = false;

    prepare_delegated_process_with(&worker_root, &mut io, |io, parent, worker_root| {
        assert_eq!(io.current_group()?, parent.join(DELEGATED_PROCESS_LEAF));
        assert!(io.direct_processes(parent)?.is_empty());
        assert_eq!(worker_root.parent(), Some(parent));
        controller_enable_called = true;
        Ok(())
    })
    .unwrap();

    assert!(controller_enable_called);
    assert_eq!(io.current, control_group);
    assert_eq!(io.ids(&control_group), vec![DAEMON_PID]);
    assert!(io.ids(&parent).is_empty());
    assert!(io.ids(&worker_root).is_empty());
    assert_eq!(worker_root.parent(), Some(parent.as_path()));
    assert!(io.groups.contains(&worker_root));
}

#[test]
fn handoff_rejects_a_mismatched_process_cgroup_probe_without_moving_any_pid() {
    let (parent, worker_root) = fixture_paths();
    let mut io = MockCgroupIo::new(&parent, &worker_root);
    io.current = parent.join("unexpected-child");
    io.groups.insert(io.current.clone());
    io.processes.insert(io.current.clone(), vec![DAEMON_PID]);

    let error = handoff(&worker_root, &mut io).unwrap_err();

    assert_eq!(error.reason_code, "CGROUP_DELEGATION_PROBE_MISMATCH");
    assert_eq!(io.ids(&io.current), vec![DAEMON_PID]);
    assert_eq!(io.ids(&parent), vec![DAEMON_PID]);
}

#[test]
fn handoff_rejects_foreign_service_process_before_moving_daemon() {
    let (parent, worker_root) = fixture_paths();
    let mut io = MockCgroupIo::new(&parent, &worker_root);
    io.processes.entry(parent.clone()).or_default().push(44_444);

    let error = handoff(&worker_root, &mut io).unwrap_err();

    assert_eq!(error.reason_code, "CGROUP_DELEGATION_PARENT_POPULATED");
    assert_eq!(io.current, parent);
    assert_eq!(io.ids(&parent), vec![DAEMON_PID, 44_444]);
}

#[test]
fn handoff_rejects_unexpected_sibling_and_unsafe_existing_control_leaf() {
    let (parent, worker_root) = fixture_paths();
    let mut io = MockCgroupIo::new(&parent, &worker_root);
    let unexpected = parent.join("unmanaged-child");
    io.groups.insert(unexpected);

    let error = handoff(&worker_root, &mut io).unwrap_err();
    assert_eq!(error.reason_code, "CGROUP_DELEGATION_CHILD_UNEXPECTED");
    assert_eq!(io.current, parent);

    let control_group = parent.join(DELEGATED_PROCESS_LEAF);
    io.groups.insert(control_group.clone());
    io.groups.remove(&parent.join("unmanaged-child"));
    io.processes.insert(control_group, vec![44_444]);

    let error = handoff(&worker_root, &mut io).unwrap_err();
    assert_eq!(error.reason_code, "CGROUP_DELEGATION_CONTROL_LEAF_UNSAFE");
    assert_eq!(io.current, parent);
    assert_eq!(io.ids(&parent), vec![DAEMON_PID]);
}

#[test]
fn handoff_rejects_move_permission_failure_and_post_move_parent_population() {
    let (parent, worker_root) = fixture_paths();
    let mut io = MockCgroupIo::new(&parent, &worker_root);
    io.deny_move = true;

    let error = handoff(&worker_root, &mut io).unwrap_err();
    assert_eq!(error.reason_code, "CGROUP_DELEGATION_MOVE_FAILED");
    assert_eq!(io.current, parent);
    assert_eq!(io.ids(&parent), vec![DAEMON_PID]);

    io.deny_move = false;
    io.leave_foreign_parent_pid = true;
    let error = handoff(&worker_root, &mut io).unwrap_err();
    assert_eq!(error.reason_code, "CGROUP_DELEGATION_VERIFY_FAILED");
    assert_eq!(io.current, parent.join(DELEGATED_PROCESS_LEAF));
    assert_eq!(io.ids(&parent), vec![44_444]);
}

#[test]
fn handoff_is_idempotent_only_when_control_leaf_contains_exactly_this_daemon() {
    let (parent, worker_root) = fixture_paths();
    let control_group = parent.join(DELEGATED_PROCESS_LEAF);
    let mut io = MockCgroupIo::new(&parent, &worker_root);
    io.groups.insert(control_group.clone());
    io.current = control_group.clone();
    io.processes.insert(control_group.clone(), vec![DAEMON_PID]);
    io.processes.insert(parent.clone(), Vec::new());

    handoff(&worker_root, &mut io).unwrap();

    assert_eq!(io.current, control_group);
    assert_eq!(io.ids(&control_group), vec![DAEMON_PID]);
    assert!(io.ids(&parent).is_empty());
}
