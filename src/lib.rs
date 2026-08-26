#![deny(unsafe_code)]

//! Foundation crate for the AI Dev Orchestrator workspace.

/// Reports whether the workspace crate is available.
#[must_use]
pub const fn is_ready() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::is_ready;

    #[test]
    fn workspace_is_ready() {
        assert!(is_ready());
    }
}
