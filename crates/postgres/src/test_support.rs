use crate::{ConnectionPolicy, SslMode};
use sqlx::{ConnectOptions, Connection, PgConnection};

pub(crate) struct Fixture {
    pub(crate) uri: String,
    pub(crate) policy: ConnectionPolicy,
    admin_uri: String,
    database: String,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let admin_uri = std::env::var("METRIC_TEST_POSTGRES_URI")
            .expect("real PostgreSQL tests require METRIC_TEST_POSTGRES_URI; no silent skip");
        let policy = ConnectionPolicy {
            ssl_mode: SslMode::Disable,
            allow_insecure_connection: true,
            ..Default::default()
        };
        let options = policy
            .connect_options(&admin_uri)
            .unwrap()
            .disable_statement_logging();
        let mut admin = PgConnection::connect_with(&options).await.unwrap();
        // Identifier is generated internally, never interpolated from a user/test URI.
        let database = format!("metric_pg_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&mut admin)
            .await
            .unwrap();
        admin.close().await.unwrap();
        let mut uri = url::Url::parse(&admin_uri).unwrap();
        uri.set_path(&database);
        Self {
            uri: uri.to_string(),
            policy,
            admin_uri,
            database,
        }
    }

    pub(crate) async fn connection(&self) -> PgConnection {
        PgConnection::connect_with(&self.policy.connect_options(&self.uri).unwrap())
            .await
            .unwrap()
    }

    pub(crate) async fn cleanup(self) {
        assert!(self.database.starts_with("metric_pg_") && self.database.len() == 42);
        assert!(
            self.database
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        );
        let mut admin =
            PgConnection::connect_with(&self.policy.connect_options(&self.admin_uri).unwrap())
                .await
                .unwrap();
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&mut admin)
            .await
            .unwrap();
        admin.close().await.unwrap();
    }
}
