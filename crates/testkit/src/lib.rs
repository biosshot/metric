//! Reusable public contract fixtures are added with their owning product phases.

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires deploy/compose.dev.yml"]
    fn infrastructure_mongodb_orchestration_is_pinned() {
        let compose = include_str!("../../../deploy/compose.dev.yml");
        assert!(compose.contains("image: mongo:8.0.12"));
    }

    #[test]
    #[ignore = "recorded only on declared benchmark hardware"]
    fn performance_empty_foundation_has_no_module_workers() {
        assert_eq!(
            std::mem::size_of::<faultkeep_application::observability::Metrics>(),
            0
        );
        let shutdown = faultkeep_application::shutdown::ShutdownRoot::new();
        assert!(!shutdown.is_started());
    }
}
