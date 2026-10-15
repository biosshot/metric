use crate::{ConnectionPolicy, PostgresDatabase, PostgresProjectStore, SslMode};
use metric_domain::{
    DisplayName, IpScrubPolicy, ItemCapabilities, OrganizationId, OrganizationIdentity,
    ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits, SecretBytes, Slug,
    Timestamp,
};
use metric_ports::ProjectStore;
use sqlx::{AssertSqlSafe, ConnectOptions, Connection, PgConnection};

/// One bounded Event/Issue fixture shared by finalization and backlog tests.
pub(crate) fn finalized(
    project: i32,
    issue_seed: u8,
    event_id: metric_domain::EventId,
    occurred: i64,
    received: i64,
    release: Option<&str>,
    environment: Option<&str>,
) -> metric_domain::finalization::FinalizeEvent {
    use metric_domain::{
        event::{EventLevel, EventPlatform},
        finalization::{FinalizeEvent, ProcessedEventPayload, SearchToken},
        grouping::{
            GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
            GroupingStrategy, derive_issue_id,
        },
        issue::{IssueCulprit, IssueGroupingDetail, IssueOccurrence, IssueRelease, IssueTitle},
    };
    let project_id = ProjectId::new(project).unwrap();
    let mut bytes = [issue_seed; 34];
    bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    let grouping_key = GroupingKey::parse(&bytes).unwrap();
    let occurred_at = Timestamp::from_unix_millis(occurred).unwrap();
    let received_at = Timestamp::from_unix_millis(received).unwrap();
    FinalizeEvent {
        project_id,event_id,occurred_at,received_at,level:EventLevel::Error,platform:EventPlatform::Rust,
        issue:IssueOccurrence { project_id,event_id,occurred_at,received_at,grouping_key,issue_id:derive_issue_id(project_id,grouping_key),release:release.map(|value|IssueRelease::new(value).unwrap()),title:IssueTitle::new(format!("Panic: synthetic Issue {issue_seed}")).unwrap(),culprit:Some(IssueCulprit::new("crate::synthetic").unwrap()),increment:std::num::NonZeroU64::MIN,grouping:IssueGroupingDetail { strategy:GroupingStrategy::Message,explanation:GroupingExplanation { summary:"normalized message".into(),components:vec![GroupingComponent { kind:GroupingComponentKind::Message,value:format!("synthetic Issue {issue_seed}").into() }] } } },
        environment:environment.map(Into::into),
        search_tokens:release.map(SearchToken::release).into_iter().chain(environment.map(SearchToken::environment)).collect(),
        payload:ProcessedEventPayload::new(serde_json::to_vec(&serde_json::json!({"event_id":event_id.to_string(),"_metric":{"symbolication":{"status":"not_required"}},"release":release,"environment":environment,"message":"processed synthetic event","timestamp":occurred,"platform":"rust"})).unwrap()),
    }
}

pub(crate) struct Fixture {
    pub(crate) uri: String,
    pub(crate) policy: ConnectionPolicy,
    admin_uri: String,
    database: String,
}

/// The same two organizations/projects exercise isolation in every capability suite.
pub(crate) fn project_identity(id: i32, organization: u64) -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(id).unwrap(),
        organization_id: OrganizationId::new(organization).unwrap(),
        slug: Slug::new("same-project").unwrap(),
        display_name: DisplayName::new("'; DROP TABLE metric.projects; -- Юникод").unwrap(),
        state: ProjectAcceptanceState::Active,
        policy_revision: 1,
        ip_policy: IpScrubPolicy::Hmac,
        items: ItemCapabilities {
            error: true,
            client_report: true,
            log: true,
            transaction: true,
            span: true,
            feedback: true,
            check_in: true,
            metric: true,
            replay: true,
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
        created_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
    }
}

pub(crate) async fn seed_projects(
    database: PostgresDatabase,
    max_keys: usize,
) -> PostgresProjectStore {
    let store = PostgresProjectStore::new(database, SecretBytes::new([42; 32]), max_keys).unwrap();
    for id in [1, 2] {
        store
            .insert_organization(OrganizationIdentity {
                id: OrganizationId::new(id).unwrap(),
                slug: Slug::new(format!("org-{id}")).unwrap(),
                display_name: DisplayName::new("Test").unwrap(),
                created_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
            })
            .await
            .unwrap();
        store
            .insert_project(project_identity(id as i32, id))
            .await
            .unwrap();
    }
    store
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
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE {database}")))
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
        sqlx::query(AssertSqlSafe(format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.database
        )))
        .execute(&mut admin)
        .await
        .unwrap();
        admin.close().await.unwrap();
    }
}

/// Native-wire fault fixture: forward every message except one successful COMMIT
/// completion. PostgreSQL has committed before emitting that response, while the
/// client sees connection loss. No credentials or payloads are logged.
pub(crate) struct CommitReplyProxy {
    pub(crate) uri: String,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shutdown: metric_application::shutdown::ShutdownRoot,
    task: tokio::task::JoinHandle<()>,
}

impl CommitReplyProxy {
    pub(crate) async fn start(target_uri: &str) -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{TcpListener, TcpStream},
            task::JoinSet,
        };
        let mut target = url::Url::parse(target_uri).unwrap();
        let host = target.host_str().unwrap().to_owned();
        let port = target.port().unwrap_or(5432);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        target.set_host(Some("127.0.0.1")).unwrap();
        target
            .set_port(Some(listener.local_addr().unwrap().port()))
            .unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let armed_task = Arc::clone(&armed);
        let dropped_task = Arc::clone(&dropped);
        let shutdown = metric_application::shutdown::ShutdownRoot::new();
        let signal = shutdown.signal();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    () = signal.cancelled() => break,
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    accepted = listener.accept() => {
                        let (client, _) = accepted.unwrap();
                        let host = host.clone();
                        let armed = Arc::clone(&armed_task);
                        let dropped = Arc::clone(&dropped_task);
                        connections.spawn(async move {
                            let server = TcpStream::connect((host.as_str(), port)).await.unwrap();
                            let (mut client_read, mut client_write) = client.into_split();
                            let (mut server_read, mut server_write) = server.into_split();
                            let incoming = tokio::io::copy(&mut client_read, &mut server_write);
                            let outgoing = async {
                                loop {
                                    let mut header = [0_u8; 5];
                                    server_read.read_exact(&mut header).await?;
                                    let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
                                    if !(4..=8 * 1024 * 1024).contains(&length) {
                                        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
                                    }
                                    let mut payload = vec![0_u8; length - 4];
                                    server_read.read_exact(&mut payload).await?;
                                    if header[0] == b'C' && payload == b"COMMIT\0" && armed.swap(false, Ordering::SeqCst) {
                                        dropped.store(true, Ordering::SeqCst);
                                        return Ok::<_, std::io::Error>(());
                                    }
                                    client_write.write_all(&header).await?;
                                    client_write.write_all(&payload).await?;
                                }
                            };
                            // Completion cancels both directions and closes the native socket.
                            tokio::select! { _ = incoming => {}, _ = outgoing => {} }
                        });
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            uri: target.to_string(),
            armed,
            dropped,
            shutdown,
            task,
        }
    }

    pub(crate) fn drop_next_commit_reply(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn did_drop_reply(&self) -> bool {
        self.dropped.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) async fn close(self) {
        self.shutdown.begin();
        self.task.await.unwrap();
    }
}
