//! Application-wide cancellation and operability contracts.

pub mod auth;
pub mod blob_cleanup;
pub mod debug_files;
pub mod deletion;
pub mod dispatcher;
pub mod finalizer;
pub mod ingest;
pub mod issues;
pub mod native_api;
pub mod normalizer;
pub mod observability;
pub mod processor;
pub mod projects;
pub mod scheduler;
pub mod search;
pub mod shutdown;
pub mod symbolication;
pub mod writer;
