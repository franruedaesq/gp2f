//! Runtime secret resolution with file-based fallback.
//!
//! ## Motivation
//!
//! Hard-coding secret values in environment variables is a common source of
//! accidental exposure (process lists, `/proc/self/environ`, log scraping).
//! The preferred Kubernetes / Docker Swarm pattern is to mount secrets as
//! files under `/run/secrets/` and consume them at runtime.
//!
//! ## Resolution order (for a given variable name `VAR`)
//!
//! 1. **Env var**: `VAR` is set → return its value.
//! 2. **File env var**: `VAR_FILE` is set → read the file at that path and
//!    return its trimmed contents.  This maps directly to Kubernetes
//!    `secretKeyRef` with `mountPath` / Docker Swarm secret mounts.
//! 3. **Not found** → return `None`.
//!
//! ## Usage
//!
//! ```rust,ignore
//! // In your factory function:
//! if let Some(url) = crate::secrets::resolve_secret("REDIS_URL") {
//!     // connect …
//! }
//! ```
//!
//! To inject via Kubernetes Secret:
//! ```yaml
//! env:
//!   - name: REDIS_URL_FILE
//!     value: /run/secrets/redis_url
//! volumeMounts:
//!   - name: redis-secret
//!     mountPath: /run/secrets/redis_url
//!     subPath: redis_url
//! ```

/// Resolve a secret by name, with a file-path fallback.
///
/// Checks `name` as an environment variable first.  If absent, checks
/// `{name}_FILE`; if that is set, reads and returns the trimmed file content.
/// Returns `None` if neither is set or if the file cannot be read.
pub fn resolve_secret(name: &str) -> Option<String> {
    // 1. Direct env var.
    if let Ok(val) = std::env::var(name) {
        return Some(val);
    }

    // 2. File-path env var (e.g. REDIS_URL_FILE=/run/secrets/redis_url).
    let file_var = format!("{name}_FILE");
    if let Ok(path) = std::env::var(&file_var) {
        match std::fs::read_to_string(&path) {
            Ok(contents) => return Some(contents.trim().to_owned()),
            Err(e) => {
                tracing::warn!(
                    var = %file_var,
                    "resolve_secret: failed to read secret file: {e}"
                );
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_direct_env_var() {
        std::env::set_var("_TEST_SECRET_DIRECT", "my-value");
        assert_eq!(
            resolve_secret("_TEST_SECRET_DIRECT"),
            Some("my-value".to_owned())
        );
        std::env::remove_var("_TEST_SECRET_DIRECT");
    }

    #[test]
    fn returns_none_when_neither_set() {
        std::env::remove_var("_TEST_SECRET_ABSENT");
        std::env::remove_var("_TEST_SECRET_ABSENT_FILE");
        assert_eq!(resolve_secret("_TEST_SECRET_ABSENT"), None);
    }

    #[test]
    fn reads_from_file_when_file_var_set() {
        let path = std::env::temp_dir().join("_gp2f_test_secret_file_var");
        std::fs::write(&path, "  file-secret  ").unwrap();
        std::env::remove_var("_TEST_SECRET_FILE_VAR");
        std::env::set_var("_TEST_SECRET_FILE_VAR_FILE", path.to_str().unwrap());
        let result = resolve_secret("_TEST_SECRET_FILE_VAR");
        std::env::remove_var("_TEST_SECRET_FILE_VAR_FILE");
        let _ = std::fs::remove_file(&path);
        assert_eq!(result, Some("file-secret".to_owned()));
    }

    #[test]
    fn direct_env_var_takes_precedence_over_file() {
        let path = std::env::temp_dir().join("_gp2f_test_secret_both");
        std::fs::write(&path, "file-value").unwrap();
        std::env::set_var("_TEST_SECRET_BOTH", "direct-value");
        std::env::set_var("_TEST_SECRET_BOTH_FILE", path.to_str().unwrap());
        let result = resolve_secret("_TEST_SECRET_BOTH");
        std::env::remove_var("_TEST_SECRET_BOTH");
        std::env::remove_var("_TEST_SECRET_BOTH_FILE");
        let _ = std::fs::remove_file(&path);
        assert_eq!(result, Some("direct-value".to_owned()));
    }
}
