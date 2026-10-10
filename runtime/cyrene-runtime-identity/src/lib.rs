// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: runtime/cyrene-runtime-identity/src/lib.rs                  ║
// ║ Module: CYRENE Platform native runtime identity selectors           ║
// ║ Role: Resolve account and group names before strict UDS peer checks. ║
// ║                                                                      ║
// ║ 模块：CYRENE Platform 原生运行时身份选择器                           ║
// ║ 职责：在严格 UDS 对端检查前解析账户和组名称。                        ║
// ╚══════════════════════════════════════════════════════════════════════╝
//! Resolve native runtime identity selectors to numeric Unix credentials.
//!
//! Numeric values remain compatible with managed unit drop-ins. Named values
//! are resolved through the host account database before callers configure
//! exact `SO_PEERCRED` policies.

use std::io;

fn resolve_selector(
    value: &str,
    kind: &str,
    lookup_name: impl FnOnce(&str) -> io::Result<Option<u32>>,
) -> io::Result<u32> {
    let unsigned = value.strip_prefix('+').unwrap_or(value);
    let signed_decimal = value.strip_prefix('-').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    });
    let unsigned_decimal =
        !unsigned.is_empty() && unsigned.bytes().all(|byte| byte.is_ascii_digit());

    if signed_decimal || unsigned_decimal {
        return unsigned.parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{kind} selector is outside the unsigned 32-bit range"),
            )
        });
    }

    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{kind} selector cannot be empty"),
        ));
    }

    lookup_name(value)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown {kind} name selector"),
        )
    })
}

/// Resolve a decimal UID or an existing account name to its numeric UID.
///
/// Signed or out-of-range decimal text is rejected as numeric input and is
/// never retried as an account name.
pub fn resolve_uid_selector(value: &str) -> io::Result<u32> {
    #[cfg(unix)]
    {
        resolve_selector(value, "UID", |name| {
            nix::unistd::User::from_name(name)
                .map(|user| user.map(|entry| entry.uid.as_raw()))
                .map_err(io::Error::other)
        })
    }

    #[cfg(not(unix))]
    {
        resolve_selector(value, "UID", |_| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "named UID selectors require a Unix host",
            ))
        })
    }
}

/// Resolve a decimal GID or an existing group name to its numeric GID.
///
/// Signed or out-of-range decimal text is rejected as numeric input and is
/// never retried as a group name.
pub fn resolve_gid_selector(value: &str) -> io::Result<u32> {
    #[cfg(unix)]
    {
        resolve_selector(value, "GID", |name| {
            nix::unistd::Group::from_name(name)
                .map(|group| group.map(|entry| entry.gid.as_raw()))
                .map_err(io::Error::other)
        })
    }

    #[cfg(not(unix))]
    {
        resolve_selector(value, "GID", |_| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "named GID selectors require a Unix host",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_gid_selector, resolve_selector, resolve_uid_selector};

    #[test]
    fn decimal_ids_remain_compatible_and_bounded() {
        assert_eq!(resolve_uid_selector("991").unwrap(), 991);
        assert_eq!(resolve_gid_selector("2764").unwrap(), 2764);
        assert_eq!(resolve_gid_selector("+2764").unwrap(), 2764);
        assert_eq!(resolve_uid_selector("4294967295").unwrap(), u32::MAX);
        assert!(resolve_gid_selector("4294967296").is_err());
        assert!(resolve_gid_selector("-1").is_err());
    }

    #[test]
    fn invalid_numeric_text_is_not_retried_as_a_name() {
        let lookup_called = std::cell::Cell::new(false);
        let lookup = |_: &str| {
            lookup_called.set(true);
            Ok(Some(17))
        };

        assert!(resolve_selector("4294967296", "GID", lookup).is_err());
        assert!(!lookup_called.get());
        assert!(resolve_selector("-1", "UID", |_| Ok(Some(17))).is_err());
    }

    #[test]
    fn named_selector_requires_a_successful_lookup() {
        assert_eq!(
            resolve_selector("cyrene", "GID", |_| Ok(Some(2764))).unwrap(),
            2764
        );
        assert!(resolve_selector("missing", "GID", |_| Ok(None)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn existing_account_and_group_names_resolve_from_the_host_database() {
        assert_eq!(resolve_uid_selector("root").unwrap(), 0);
        assert_eq!(resolve_gid_selector("root").unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn unknown_names_fail_closed() {
        assert!(resolve_uid_selector("cyrene-identity-no-such-user-9f338f").is_err());
        assert!(resolve_gid_selector("cyrene-identity-no-such-group-9f338f").is_err());
        assert!(resolve_uid_selector("").is_err());
        assert!(resolve_gid_selector("").is_err());
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_hosts_keep_numeric_ids_and_reject_names() {
        assert_eq!(resolve_uid_selector("991").unwrap(), 991);
        assert_eq!(resolve_gid_selector("2764").unwrap(), 2764);
        assert!(resolve_uid_selector("cyrene-kernel").is_err());
        assert!(resolve_gid_selector("cyrene").is_err());
    }
}
