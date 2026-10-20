//! Holds MongoDB commands of a test client, so a test can order them against others.

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use mongodb::{
    Client,
    event::{EventHandler, command::CommandEvent},
    options::ClientOptions,
};

/// Holds the first command it matches until released.
#[derive(Default)]
pub struct Gate {
    armed: AtomicBool,
    pub entered: tokio::sync::Notify,
    open: Mutex<bool>,
    wakeup: Condvar,
}

impl Gate {
    pub fn armed() -> Arc<Self> {
        let gate = Self::default();
        gate.armed.store(true, Ordering::SeqCst);
        Arc::new(gate)
    }

    fn hold(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let _ = self
                .wakeup
                .wait_timeout_while(self.open.lock().unwrap(), Duration::from_secs(30), |open| {
                    !*open
                })
                .unwrap();
        }
    }

    pub fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.wakeup.notify_all();
    }
}

/// A client whose `(command, collection)` commands wait at the paired gate.
pub async fn gated_client(
    uri: &str,
    gates: Vec<(&'static str, &'static str, Arc<Gate>)>,
) -> mongodb::error::Result<Client> {
    let mut options = ClientOptions::parse(uri).await?;
    options.command_event_handler = Some(EventHandler::callback(move |event: CommandEvent| {
        if let CommandEvent::Started(event) = event {
            for (command, collection, gate) in &gates {
                if event.command_name == *command
                    && event.command.get_str(command) == Ok(collection)
                {
                    gate.hold();
                }
            }
        }
    }));
    Client::with_options(options)
}
