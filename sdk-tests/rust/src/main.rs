use std::{error::Error, fmt, process, thread, time::Duration};

#[derive(Debug)]
struct FaultkeepRustSdkCompatibilityError;

impl fmt::Display for FaultkeepRustSdkCompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Faultkeep real Rust SDK compatibility event")
    }
}

impl Error for FaultkeepRustSdkCompatibilityError {}

fn main() {
    let Some(dsn) = std::env::args().nth(1) else {
        eprintln!("Faultkeep DSN argument is required");
        process::exit(2);
    };
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(15));
        eprintln!("real Rust SDK sender exceeded its 15 second process deadline");
        process::exit(2);
    });

    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(dsn.parse().expect("Faultkeep DSN must be valid")),
        environment: Some("sdk-compatibility".into()),
        release: Some("faultkeep-rust-sdk-test@1.0.0".into()),
        traces_sample_rate: 0.0,
        send_default_pii: false,
        auto_session_tracking: false,
        attach_stacktrace: true,
        shutdown_timeout: Duration::from_secs(8),
        ..Default::default()
    });
    sentry::configure_scope(|scope| {
        scope.set_tag("faultkeep.sdk_test", "rust");
    });

    let event_id = sentry::capture_error(&FaultkeepRustSdkCompatibilityError);
    let flushed = guard.close(Some(Duration::from_secs(8)));
    drop(guard);
    if !flushed {
        eprintln!("the real Rust SDK did not flush the captured Event");
        process::exit(2);
    }
    println!(
        "{}",
        serde_json::json!({
            "event_id": event_id.to_string(),
            "flushed": flushed
        })
    );
}
