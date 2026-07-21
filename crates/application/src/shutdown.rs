use tokio_util::sync::CancellationToken;

/// The single owner allowed to fence new work and start graceful shutdown.
#[derive(Debug)]
pub struct ShutdownRoot {
    token: CancellationToken,
}

impl ShutdownRoot {
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    #[must_use]
    pub fn signal(&self) -> ShutdownSignal {
        ShutdownSignal {
            token: self.token.clone(),
        }
    }

    pub fn begin(&self) {
        self.token.cancel();
    }

    #[must_use]
    pub fn is_started(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl Default for ShutdownRoot {
    fn default() -> Self {
        Self::new()
    }
}

/// Cloneable, read-only cancellation primitive passed to composed modules.
#[derive(Debug, Clone)]
pub struct ShutdownSignal {
    token: CancellationToken,
}

impl ShutdownSignal {
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn root_cancels_all_read_only_signals() {
        let root = ShutdownRoot::new();
        let signal = root.signal();
        root.begin();
        signal.cancelled().await;
        assert!(signal.is_cancelled());
    }
}
