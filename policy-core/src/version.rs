use thiserror::Error;

/// Version policy controls which AST versions are accepted.
#[derive(Debug, Clone)]
pub struct VersionPolicy {
    /// The set of allowed AST version strings (semver) for this workflow.
    allowed: Vec<String>,
}

/// Error returned when a client presents an AST version that is not allowed.
#[derive(Debug, Error)]
#[error("AST version '{client_version}' is not in the allowed set for this workflow: {allowed:?}")]
pub struct VersionMismatch {
    pub client_version: String,
    pub allowed: Vec<String>,
}

impl VersionPolicy {
    /// Create a policy that accepts any of the given `versions`.
    pub fn new(versions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowed: versions.into_iter().map(|v| v.into()).collect(),
        }
    }

    /// Returns `Ok(())` if `version` is in the allowed set, otherwise
    /// returns a [`VersionMismatch`] error.
    pub fn check(&self, version: &str) -> Result<(), VersionMismatch> {
        if self.allowed.iter().any(|a| a == version) {
            Ok(())
        } else {
            Err(VersionMismatch {
                client_version: version.to_owned(),
                allowed: self.allowed.clone(),
            })
        }
    }

    /// Returns `true` if `version` is accepted.
    pub fn allows(&self, version: &str) -> bool {
        self.check(version).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_listed_version() {
        let policy = VersionPolicy::new(["1.0.0", "1.1.0"]);
        assert!(policy.allows("1.0.0"));
        assert!(policy.allows("1.1.0"));
    }

    #[test]
    fn rejects_unlisted_version() {
        let policy = VersionPolicy::new(["1.0.0"]);
        assert!(!policy.allows("2.0.0"));
    }

    #[test]
    fn check_ok() {
        let policy = VersionPolicy::new(["1.0.0"]);
        assert!(policy.check("1.0.0").is_ok());
    }

    #[test]
    fn check_err_contains_version() {
        let policy = VersionPolicy::new(["1.0.0"]);
        let err = policy.check("9.9.9").unwrap_err();
        assert!(err.to_string().contains("9.9.9"));
    }
}
