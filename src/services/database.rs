use crate::{
    models::*,
    services::{pgpass, server::LocalServer},
    sql::{self, Mutation, browse_sql, lexer, quote_ident},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, pin_mut};
use postgres_native_tls::MakeTlsConnector;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio_postgres::{AsyncMessage, Client, Config, SimpleQueryMessage, types::ToSql};

pub const MAX_RESULT_ROWS: usize = 5_000;
/// The library and extension behind the slow query list.
pub const STATEMENTS_LIBRARY: &str = "pg_stat_statements";
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESULTS: usize = 64;
const DATA_STATEMENT_TIMEOUT_MS: u64 = 30_000;

/// The table browser and the SQL editor use separate sessions so that grid edits
/// never commit or join a transaction opened in the SQL editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Data,
    Sql,
}

pub struct Database {
    pub client: Client,
    pub profile: ConnectionProfile,
    pub kind: SessionKind,
    backend_pid: i32,
    connection: tokio::task::JoinHandle<()>,
    notices: Mutex<mpsc::Receiver<String>>,
}

impl Drop for Database {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

/// TLS settings for a profile. Prefer and Require encrypt without authenticating
/// the server, like libpq. Verify CA checks the certificate chain; Verify Full also
/// checks the host name. Extra root certificates and a client identity are optional.
pub fn tls(profile: &ConnectionProfile) -> Result<MakeTlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();
    match profile.ssl {
        SslMode::Prefer | SslMode::Require => {
            builder
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        SslMode::VerifyCa => {
            builder.danger_accept_invalid_hostnames(true);
        }
        SslMode::Disable | SslMode::VerifyFull => {}
    }
    let root = profile.ssl_root_cert.trim();
    if !root.is_empty() {
        let pem = std::fs::read(root)
            .with_context(|| format!("Cannot read the CA certificate file {root}."))?;
        let mut found = false;
        for block in pem_blocks(&pem, "CERTIFICATE") {
            builder.add_root_certificate(
                native_tls::Certificate::from_pem(&block)
                    .context("The CA certificate file is not valid PEM.")?,
            );
            found = true;
        }
        ensure!(
            found,
            "The CA certificate file contains no PEM certificate."
        );
    }
    let cert = profile.ssl_client_cert.trim();
    let key = profile.ssl_client_key.trim();
    if !cert.is_empty() {
        let cert_pem = std::fs::read(cert)
            .with_context(|| format!("Cannot read the client certificate file {cert}."))?;
        let key_pem = std::fs::read(key)
            .with_context(|| format!("Cannot read the client key file {key}."))?;
        let identity = native_tls::Identity::from_pkcs8(&cert_pem, &key_pem).context(
            "The client certificate or key is not valid. The key must be an unencrypted PKCS#8 PEM file (BEGIN PRIVATE KEY).",
        )?;
        builder.identity(identity);
    }
    Ok(MakeTlsConnector::new(builder.build()?))
}

fn pem_blocks(pem: &[u8], label: &str) -> Vec<Vec<u8>> {
    let text = String::from_utf8_lossy(pem);
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut blocks = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find(&begin) {
        let Some(stop) = rest[start..].find(&end) else {
            break;
        };
        let stop = start + stop + end.len();
        blocks.push(rest.as_bytes()[start..stop].to_vec());
        rest = &rest[stop..];
    }
    blocks
}

impl Database {
    pub async fn connect(profile: ConnectionProfile, password: &str) -> Result<Self> {
        Self::connect_session(profile, password, SessionKind::Data, None).await
    }

    /// Connects one session. `tunnel_port` routes the TCP connection through a local
    /// SSH forward while TLS still verifies the configured host name.
    pub async fn connect_session(
        profile: ConnectionProfile,
        password: &str,
        kind: SessionKind,
        tunnel_port: Option<u16>,
    ) -> Result<Self> {
        profile.validate()?;
        let host = profile.host.trim();
        let mut config = Config::new();
        config
            .host(host)
            .port(profile.port)
            .dbname(profile.database.trim())
            .user(profile.username.trim())
            .application_name(match kind {
                SessionKind::Data => "Postern",
                SessionKind::Sql => "Postern SQL",
            })
            .connect_timeout(Duration::from_secs(10))
            .keepalives_idle(Duration::from_secs(60));
        let mut options = match kind {
            SessionKind::Data => format!(
                "-c statement_timeout={DATA_STATEMENT_TIMEOUT_MS} -c lock_timeout=5000 -c idle_in_transaction_session_timeout=60000"
            ),
            SessionKind::Sql => format!(
                "-c statement_timeout={} -c idle_in_transaction_session_timeout=0",
                u64::from(profile.statement_timeout_secs) * 1000
            ),
        };
        if profile.read_only {
            options.push_str(" -c default_transaction_read_only=on");
        }
        let extra = profile.connection_options.trim();
        if !extra.is_empty() {
            options.push(' ');
            options.push_str(extra);
        }
        config.options(&options);
        if !password.is_empty() {
            config.password(password);
        } else if let Some(password) = pgpass::lookup(
            host,
            profile.port,
            profile.database.trim(),
            profile.username.trim(),
        ) {
            config.password(password);
        }
        config.ssl_mode(match profile.ssl {
            SslMode::Disable => tokio_postgres::config::SslMode::Disable,
            SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            _ => tokio_postgres::config::SslMode::Require,
        });
        if let Some(port) = tunnel_port {
            config.hostaddr(IpAddr::V4(Ipv4Addr::LOCALHOST)).port(port);
        }
        let (client, connection) =
            tokio::time::timeout(Duration::from_secs(12), config.connect(tls(&profile)?))
                .await
                .context("Connection timed out.")?
                .context("PostgreSQL connection failed")?;
        // Bounded so that notices from sessions that never run scripts cannot grow
        // without limit; excess notices are dropped.
        let (notice_sender, notices) = mpsc::sync_channel(256);
        let connection = tokio::spawn(async move {
            let mut connection = connection;
            let messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            pin_mut!(messages);
            while let Some(message) = messages.next().await {
                match message {
                    Ok(AsyncMessage::Notice(notice)) => {
                        let mut text = format!("{}: {}", notice.severity(), notice.message());
                        if let Some(detail) = notice.detail() {
                            text += &format!("\n{detail}");
                        }
                        if let Some(hint) = notice.hint() {
                            text += &format!("\nHint: {hint}");
                        }
                        let _ = notice_sender.try_send(text);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        let mut database = Self {
            client,
            profile,
            kind,
            backend_pid: 0,
            connection,
            notices: Mutex::new(notices),
        };
        database.backend_pid = database
            .client
            .query_one("SELECT pg_catalog.pg_backend_pid()", &[])
            .await?
            .get(0);
        let startup = database.profile.startup_sql.trim().to_owned();
        if !startup.is_empty() {
            database
                .client
                .batch_execute(&startup)
                .await
                .map_err(|error| anyhow::anyhow!(error_message(&error)))
                .context("The profile's startup SQL failed")?;
        }
        database.take_notices();
        Ok(database)
    }

    pub fn backend_pid(&self) -> i32 {
        self.backend_pid
    }

    /// NOTICE and WARNING messages received since the last call.
    pub fn take_notices(&self) -> Vec<String> {
        self.notices
            .lock()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default()
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        let rows = self
            .client
            .query(
                "SELECT datname FROM pg_catalog.pg_database \
             WHERE datallowconn AND datconnlimit <> -2 \
             AND pg_catalog.has_database_privilege(oid, 'CONNECT') ORDER BY datname",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    pub async fn discover_databases(
        profile: ConnectionProfile,
        password: &str,
    ) -> Result<Vec<String>> {
        Self::discover_databases_via(profile, password, None).await
    }

    pub async fn discover_databases_via(
        profile: ConnectionProfile,
        password: &str,
        tunnel_port: Option<u16>,
    ) -> Result<Vec<String>> {
        let mut candidates = vec![];
        for name in [profile.database.trim(), "postgres", "template1"] {
            if !name.is_empty() && !candidates.contains(&name) {
                candidates.push(name);
            }
        }
        let mut last_error = None;
        for name in candidates {
            let mut candidate = profile.clone();
            candidate.database = name.into();
            match Self::connect_session(candidate, password, SessionKind::Data, tunnel_port).await {
                Ok(connection) => return connection.databases().await,
                Err(error) => {
                    let retry = error
                        .downcast_ref::<tokio_postgres::Error>()
                        .and_then(|error| error.code())
                        .is_some_and(|code| matches!(code.code(), "3D000" | "42501"));
                    if !retry {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.expect("There is always a bootstrap database"))
            .context("Could not list databases. Enter a database you can connect to and retry Show Databases.")
    }

    /// Fails when another session of this application is inside a transaction.
    /// `action` completes "before ...", such as "switching databases".
    pub async fn ensure_session_idle(&self, session: &Self, action: &str) -> Result<()> {
        match self.session_state(session.backend_pid).await? {
            TransactionState::Open | TransactionState::Failed => {
                bail!("Finish the SQL transaction with COMMIT or ROLLBACK before {action}.")
            }
            TransactionState::Idle => Ok(()),
            TransactionState::Unknown => {
                bail!("Cannot verify the SQL session state. Reconnect before {action}.")
            }
        }
    }

    /// Reads another backend's transaction state from `pg_stat_activity`. Probing
    /// from a separate session never changes the observed session's transaction.
    pub async fn session_state(&self, pid: i32) -> Result<TransactionState> {
        let row = self
            .client
            .query_opt(
                "SELECT state FROM pg_catalog.pg_stat_activity WHERE pid = $1",
                &[&pid],
            )
            .await?;
        let state = row.and_then(|row| row.get::<_, Option<String>>(0));
        Ok(match state.as_deref() {
            Some("idle") => TransactionState::Idle,
            Some("idle in transaction") => TransactionState::Open,
            Some("idle in transaction (aborted)") => TransactionState::Failed,
            _ => TransactionState::Unknown,
        })
    }

    pub async fn relations(&self) -> Result<Vec<Relation>> {
        let rows = self.client.query(
            "SELECT n.nspname, c.relname, c.relkind::text, GREATEST(c.reltuples::bigint, 0), pn.nspname, pc.relname \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_catalog.pg_inherits i ON c.relispartition AND i.inhrelid = c.oid \
             LEFT JOIN pg_catalog.pg_class pc ON pc.oid = i.inhparent \
             LEFT JOIN pg_catalog.pg_namespace pn ON pn.oid = pc.relnamespace \
             WHERE c.relkind IN ('r','p','v','m','f') AND n.nspname NOT IN ('pg_catalog','information_schema') \
             AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp_%' \
             ORDER BY n.nspname, c.relname", &[]).await?;
        Ok(rows
            .iter()
            .map(|r| Relation {
                schema: r.get(0),
                name: r.get(1),
                kind: r.get(2),
                estimated_rows: r.get(3),
                parent: r
                    .get::<_, Option<String>>(4)
                    .zip(r.get::<_, Option<String>>(5)),
            })
            .collect())
    }

    /// Finds index and table problems from the catalog and usage statistics.
    pub async fn index_report(&self) -> Result<IndexReport> {
        let user_schemas = "n.nspname NOT IN ('pg_catalog', 'information_schema') \
             AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp_%'";
        // Indexes that enforce a constraint or belong to a partitioned index are
        // never proposed for dropping.
        let droppable = "NOT i.indisprimary AND NOT i.indisunique \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c WHERE c.conindid = i.indexrelid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = i.indexrelid)";
        // An index serves a foreign key when its leading columns are the key's
        // columns in any order.
        let serves_key = "(SELECT array_agg(x ORDER BY x) FROM unnest((string_to_array(i.indkey::text, ' ')::int2[])[1:cardinality(c.conkey)]) x) \
             = (SELECT array_agg(x ORDER BY x) FROM unnest(c.conkey) x)";
        let mut items = Vec::new();
        let rows = self
            .client
            .query(
                &format!(
                    "SELECT n.nspname, t.relname, ci.relname, pg_catalog.pg_relation_size(i.indexrelid) \
                     FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ci ON ci.oid = i.indexrelid \
                     JOIN pg_catalog.pg_class t ON t.oid = i.indrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     WHERE NOT i.indisvalid AND {user_schemas} ORDER BY 4 DESC"
                ),
                &[],
            )
            .await?;
        for row in &rows {
            let (schema, table, index): (String, String, String) =
                (row.get(0), row.get(1), row.get(2));
            items.push(IndexAdvice {
                kind: AdviceKind::InvalidIndex,
                detail: "Not valid; rebuild it or drop it.".into(),
                bytes: row.get(3),
                fix: Some(format!(
                    "REINDEX INDEX CONCURRENTLY {}.{}",
                    quote_ident(&schema),
                    quote_ident(&index)
                )),
                schema,
                table,
                object: index,
            });
        }
        let rows = self
            .client
            .query(
                &format!(
                    "SELECT n.nspname, t.relname, c.conname, \
                     ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY k(attnum, ord) \
                           JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum ORDER BY k.ord), \
                     GREATEST(t.reltuples::bigint, 0), fn.nspname, ft.relname, t.relkind = 'p' \
                     FROM pg_catalog.pg_constraint c \
                     JOIN pg_catalog.pg_class t ON t.oid = c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     JOIN pg_catalog.pg_class ft ON ft.oid = c.confrelid JOIN pg_catalog.pg_namespace fn ON fn.oid = ft.relnamespace \
                     WHERE c.contype = 'f' AND c.conparentid = 0 AND {user_schemas} \
                     AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
                       WHERE i.indrelid = c.conrelid AND i.indpred IS NULL AND {serves_key}) \
                     ORDER BY 5 DESC, 1, 2"
                ),
                &[],
            )
            .await?;
        for row in &rows {
            let (schema, table): (String, String) = (row.get(0), row.get(1));
            let columns: Vec<String> = row.get(3);
            let estimated: i64 = row.get(4);
            let (referenced_schema, referenced): (String, String) = (row.get(5), row.get(6));
            items.push(IndexAdvice {
                kind: AdviceKind::ForeignKeyWithoutIndex,
                detail: format!(
                    "({}) references {referenced_schema}.{referenced}; about {estimated} rows.",
                    columns.join(", ")
                ),
                bytes: None,
                fix: Some(sql::foreign_key_index_sql(
                    &schema,
                    &table,
                    &columns,
                    row.get(7),
                )),
                schema,
                table,
                object: row.get(2),
            });
        }
        let rows = self
            .client
            .query(
                &format!(
                    "SELECT n.nspname, t.relname, a.relname, b.relname, pg_catalog.pg_relation_size(i.indexrelid), \
                     i.indkey::text = j.indkey::text, a.relkind = 'I' \
                     FROM pg_catalog.pg_index i \
                     JOIN pg_catalog.pg_index j ON j.indrelid = i.indrelid AND j.indexrelid <> i.indexrelid \
                     JOIN pg_catalog.pg_class a ON a.oid = i.indexrelid JOIN pg_catalog.pg_class b ON b.oid = j.indexrelid \
                     JOIN pg_catalog.pg_class t ON t.oid = i.indrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     WHERE {droppable} AND {user_schemas} AND a.relam = b.relam AND i.indisvalid AND j.indisvalid \
                     AND i.indexprs IS NULL AND j.indexprs IS NULL AND i.indpred IS NULL AND j.indpred IS NULL \
                     AND ((i.indkey::text = j.indkey::text AND i.indclass::text = j.indclass::text \
                           AND i.indcollation::text = j.indcollation::text AND i.indoption::text = j.indoption::text \
                           AND (j.indisunique OR j.indisprimary OR i.indexrelid > j.indexrelid)) \
                          OR (a.relam = (SELECT oid FROM pg_catalog.pg_am WHERE amname = 'btree') \
                              AND j.indkey::text LIKE i.indkey::text || ' %' \
                              AND j.indclass::text LIKE i.indclass::text || ' %' \
                              AND j.indcollation::text LIKE i.indcollation::text || ' %')) \
                     ORDER BY 5 DESC, 1, 2, 3, 6 DESC, 4"
                ),
                &[],
            )
            .await?;
        let mut reported = std::collections::BTreeSet::new();
        for row in &rows {
            let (schema, table, index, other): (String, String, String, String) =
                (row.get(0), row.get(1), row.get(2), row.get(3));
            if !reported.insert((schema.clone(), index.clone())) {
                continue;
            }
            let exact: bool = row.get(5);
            let partitioned: bool = row.get(6);
            items.push(IndexAdvice {
                kind: if exact {
                    AdviceKind::DuplicateIndex
                } else {
                    AdviceKind::RedundantIndex
                },
                detail: if exact {
                    format!("Same columns as {other}.")
                } else {
                    format!("{other} starts with the same columns.")
                },
                bytes: row.get(4),
                fix: Some(sql::drop_index_sql(&schema, &index, partitioned)),
                schema,
                table,
                object: index,
            });
        }
        let rows = self
            .client
            .query(
                &format!(
                    "SELECT n.nspname, t.relname, ci.relname, pg_catalog.pg_relation_size(i.indexrelid) \
                     FROM pg_catalog.pg_stat_user_indexes s JOIN pg_catalog.pg_index i ON i.indexrelid = s.indexrelid \
                     JOIN pg_catalog.pg_class ci ON ci.oid = i.indexrelid JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     WHERE s.idx_scan = 0 AND i.indisvalid AND {droppable} AND {user_schemas} \
                     AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c \
                       WHERE c.contype = 'f' AND c.conrelid = i.indrelid AND i.indkey[0] = ANY (c.conkey)) \
                     ORDER BY 4 DESC, 1, 2, 3"
                ),
                &[],
            )
            .await?;
        for row in &rows {
            let (schema, table, index): (String, String, String) =
                (row.get(0), row.get(1), row.get(2));
            if reported.contains(&(schema.clone(), index.clone())) {
                continue;
            }
            items.push(IndexAdvice {
                kind: AdviceKind::UnusedIndex,
                detail: "No index scans.".into(),
                bytes: row.get(3),
                fix: Some(sql::drop_index_sql(&schema, &index, false)),
                schema,
                table,
                object: index,
            });
        }
        let rows = self
            .client
            .query(
                "SELECT schemaname, relname, seq_scan, seq_tup_read, COALESCE(idx_scan, 0), n_live_tup \
                 FROM pg_catalog.pg_stat_user_tables \
                 WHERE seq_scan > 0 AND n_live_tup >= 10000 AND seq_tup_read / seq_scan >= 10000 \
                 AND seq_scan > COALESCE(idx_scan, 0) ORDER BY seq_tup_read DESC LIMIT 25",
                &[],
            )
            .await?;
        for row in &rows {
            let (scans, read, index_scans, live): (i64, i64, i64, i64) =
                (row.get(2), row.get(3), row.get(4), row.get(5));
            items.push(IndexAdvice {
                kind: AdviceKind::SequentialScans,
                schema: row.get(0),
                table: row.get(1),
                object: String::new(),
                detail: format!(
                    "{scans} sequential scans read {} rows each on average; {index_scans} index scans; about {live} rows.",
                    read / scans.max(1)
                ),
                bytes: None,
                fix: None,
            });
        }
        let rows = self
            .client
            .query(
                "SELECT schemaname, relname, n_dead_tup, n_live_tup, \
                 to_char(GREATEST(last_vacuum, last_autovacuum), 'YYYY-MM-DD HH24:MI') \
                 FROM pg_catalog.pg_stat_user_tables \
                 WHERE n_dead_tup >= 10000 AND n_dead_tup > 0.2 * GREATEST(n_live_tup, 1) \
                 ORDER BY n_dead_tup DESC LIMIT 25",
                &[],
            )
            .await?;
        for row in &rows {
            let (schema, table): (String, String) = (row.get(0), row.get(1));
            let (dead, live): (i64, i64) = (row.get(2), row.get(3));
            let vacuumed: Option<String> = row.get(4);
            items.push(IndexAdvice {
                kind: AdviceKind::DeadRows,
                detail: format!(
                    "{dead} dead and {live} live rows; last vacuum {}.",
                    vacuumed.as_deref().unwrap_or("never")
                ),
                bytes: None,
                fix: Some(format!(
                    "VACUUM (ANALYZE) {}.{}",
                    quote_ident(&schema),
                    quote_ident(&table)
                )),
                schema,
                table,
                object: String::new(),
            });
        }
        items.sort_by_key(|item| item.kind);
        let stats_since = self
            .client
            .query_one(
                "SELECT to_char(stats_reset, 'YYYY-MM-DD HH24:MI') FROM pg_catalog.pg_stat_database \
                 WHERE datname = current_database()",
                &[],
            )
            .await?
            .get(0);
        Ok(IndexReport { items, stats_since })
    }

    /// The schema of the `pg_stat_statements` extension, when it is installed.
    async fn statements_schema(&self) -> Result<Option<String>> {
        Ok(self
            .client
            .query_opt(
                "SELECT n.nspname FROM pg_catalog.pg_extension e \
                 JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'pg_stat_statements'",
                &[],
            )
            .await?
            .map(|row| row.get(0)))
    }

    /// The most expensive statements from `pg_stat_statements`.
    pub async fn slow_queries(&self, request: SlowQueryRequest) -> Result<SlowQueryReport> {
        let version: i32 = self
            .client
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .await?
            .get(0);
        let mut report = SlowQueryReport {
            generic_plans: version >= 160000,
            ..Default::default()
        };
        let facts = self.preload_facts().await?;
        let preloaded = facts
            .running
            .as_deref()
            .map(|running| sql::loads_library(running, STATEMENTS_LIBRARY));
        // Logins that cannot read shared_preload_libraries try the view and see
        // its error instead.
        let schema = self
            .statements_schema()
            .await?
            .filter(|_| preloaded != Some(false));
        let Some(schema) = schema else {
            report.status = StatementsStatus::Setup(self.statements_setup(&facts).await);
            return Ok(report);
        };
        let view = format!("{}.pg_stat_statements", quote_ident(&schema));
        let (total, mean, max) = if version >= 130000 {
            ("total_exec_time", "mean_exec_time", "max_exec_time")
        } else {
            ("total_time", "mean_time", "max_time")
        };
        let ordering = match request.order {
            SlowQueryOrder::TotalTime => "s.total",
            SlowQueryOrder::MeanTime => "s.mean",
            SlowQueryOrder::Calls => "s.calls",
            SlowQueryOrder::Rows => "s.rows",
        };
        let mut conditions = Vec::new();
        if !request.all_databases {
            conditions.push("d.datname = current_database()");
        }
        if !request.include_catalog {
            conditions.push(r"p.query !~* '\m(pg_catalog|information_schema)\.'");
        }
        let scope = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT s.query, s.calls, s.total, s.mean, s.max, s.rows, \
             100.0 * s.total / NULLIF(sum(s.total) OVER (), 0), s.hit, s.usename, s.datname \
             FROM (SELECT p.query, p.calls, p.{total} AS total, p.{mean} AS mean, p.{max} AS max, p.rows, \
                          p.shared_blks_hit::float8 / NULLIF(p.shared_blks_hit + p.shared_blks_read, 0) AS hit, \
                          COALESCE(r.rolname::text, '') AS usename, COALESCE(d.datname::text, '') AS datname \
                   FROM {view} p LEFT JOIN pg_catalog.pg_roles r ON r.oid = p.userid \
                   LEFT JOIN pg_catalog.pg_database d ON d.oid = p.dbid {scope}) s \
             ORDER BY {ordering} DESC LIMIT 200"
        );
        let rows = match self.client.query(&sql, &[]).await {
            Ok(rows) => rows,
            Err(error) => {
                report.status = StatementsStatus::Unavailable(error_message(&error));
                return Ok(report);
            }
        };
        report.queries = rows
            .iter()
            .map(|row| SlowQuery {
                query: row.get(0),
                calls: row.get(1),
                total_ms: row.get(2),
                mean_ms: row.get(3),
                max_ms: row.get(4),
                rows: row.get(5),
                share: row.get::<_, Option<f64>>(6).unwrap_or_default(),
                hit_ratio: row.get(7),
                user: row.get(8),
                database: row.get(9),
            })
            .collect();
        if version >= 140000 {
            report.stats_since = self
                .client
                .query_opt(
                    &format!(
                        "SELECT to_char(stats_reset, 'YYYY-MM-DD HH24:MI') FROM {}.pg_stat_statements_info",
                        quote_ident(&schema)
                    ),
                    &[],
                )
                .await
                .ok()
                .flatten()
                .and_then(|row| row.get(0));
        }
        Ok(report)
    }

    /// What the server reports about `shared_preload_libraries` and
    /// `pg_stat_statements`. Superusers also get the configured value, the data
    /// directory, and the library directories.
    pub async fn preload_facts(&self) -> Result<PreloadFacts> {
        let row = self
            .client
            .query_one(
                "SELECT current_setting('is_superuser') = 'on', \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_available_extensions WHERE name = 'pg_stat_statements'), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_extension WHERE extname = 'pg_stat_statements'), \
                 pg_catalog.pg_backend_pid(), \
                 (SELECT setting FROM pg_catalog.pg_settings WHERE name = 'shared_preload_libraries')",
                &[],
            )
            .await?;
        let running: Option<String> = row.get(4);
        let mut facts = PreloadFacts {
            superuser: row.get(0),
            available: row.get(1),
            installed: row.get(2),
            backend_pid: row.get(3),
            running: running.as_deref().map(sql::parse_library_list),
            configured: None,
            data_directory: None,
            library_directories: Vec::new(),
        };
        if facts.superuser {
            // The last entry in the configuration files wins; a changed value that
            // waits for a restart is listed with an error, so errors are kept.
            let row = self
                .client
                .query_one(
                    "SELECT current_setting('data_directory'), current_setting('dynamic_library_path'), \
                     (SELECT setting FROM pg_catalog.pg_config WHERE name = 'PKGLIBDIR'), \
                     f.setting, COALESCE(f.sourcefile LIKE '%postgresql.auto.conf', false) \
                     FROM (SELECT 1) AS one LEFT JOIN LATERAL ( \
                       SELECT setting, sourcefile FROM pg_catalog.pg_file_settings \
                       WHERE name = 'shared_preload_libraries' ORDER BY seqno DESC LIMIT 1) AS f ON true",
                    &[],
                )
                .await?;
            let package: Option<String> = row.get(2);
            let package = package.unwrap_or_default();
            let search: String = row.get(1);
            facts.library_directories = std::iter::once(package.clone())
                .chain(
                    search
                        .split(':')
                        .map(|entry| entry.trim().replace("$libdir", &package)),
                )
                .filter(|entry| !entry.is_empty())
                .collect();
            facts.data_directory = Some(row.get(0));
            let configured: Option<String> = row.get(3);
            facts.configured = Some(ConfiguredPreload {
                libraries: configured
                    .as_deref()
                    .map(sql::parse_library_list)
                    .unwrap_or_default(),
                in_auto_conf: row.get(4),
            });
        }
        Ok(facts)
    }

    async fn statements_setup(&self, facts: &PreloadFacts) -> StatementsSetup {
        let preloaded = facts
            .running
            .as_deref()
            .is_some_and(|running| sql::loads_library(running, STATEMENTS_LIBRARY));
        let configured = facts.configured.as_ref().map_or(preloaded, |configured| {
            sql::loads_library(&configured.libraries, STATEMENTS_LIBRARY)
        });
        let configure_blocker = if !facts.available {
            Some("This server does not include pg_stat_statements. Install the PostgreSQL contrib package on the server.".into())
        } else if !facts.superuser {
            Some("Changing shared_preload_libraries requires a superuser.".into())
        } else {
            None
        };
        let restart = if preloaded {
            RestartSupport::Unavailable("The server already loads pg_stat_statements.".into())
        } else {
            self.restart_support(facts).await
        };
        StatementsSetup {
            preloaded,
            installed: facts.installed,
            configured,
            configure_blocker,
            restart,
        }
    }

    async fn restart_support(&self, facts: &PreloadFacts) -> RestartSupport {
        let Some(directory) = &facts.data_directory else {
            return RestartSupport::Unavailable(
                "Restarting the server from Postern requires a superuser.".into(),
            );
        };
        let server = match LocalServer::detect(directory, facts.backend_pid).await {
            Ok(server) => server,
            Err(reason) => return RestartSupport::Unavailable(reason),
        };
        if !crate::services::server::library_installed(
            &facts.library_directories,
            STATEMENTS_LIBRARY,
        ) {
            return RestartSupport::Unavailable(format!(
                "The pg_stat_statements library was not found in {}.",
                facts.library_directories.join(", ")
            ));
        }
        RestartSupport::Available {
            data_directory: directory.clone(),
            log_file: server.log_file().display().to_string(),
            other_sessions: self.other_sessions().await.unwrap_or_default(),
        }
    }

    /// Client sessions that are neither this one nor another Postern session.
    pub async fn other_sessions(&self) -> Result<i64> {
        Ok(self
            .client
            .query_one(
                "SELECT count(*) FROM pg_catalog.pg_stat_activity \
                 WHERE backend_type = 'client backend' AND pid <> pg_catalog.pg_backend_pid() \
                 AND COALESCE(application_name, '') NOT LIKE 'Postern%'",
                &[],
            )
            .await?
            .get(0))
    }

    /// Writes `shared_preload_libraries` to `postgresql.auto.conf`. It takes effect
    /// when the server restarts.
    pub async fn configure_preload(&self, libraries: &[String]) -> Result<()> {
        self.client
            .batch_execute(&sql::alter_preload_sql(libraries))
            .await?;
        Ok(())
    }

    /// Restores the configured `shared_preload_libraries` recorded in `facts`.
    pub async fn restore_preload(&self, previous: &ConfiguredPreload) -> Result<()> {
        if previous.in_auto_conf {
            self.configure_preload(&previous.libraries).await
        } else {
            self.configure_preload(&[]).await
        }
    }

    /// Creates the `pg_stat_statements` extension. The server must already load
    /// its library through `shared_preload_libraries`.
    pub async fn enable_query_stats(&self) -> Result<()> {
        self.client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .await?;
        Ok(())
    }

    /// Clears the statistics collected by `pg_stat_statements`.
    pub async fn reset_query_stats(&self) -> Result<()> {
        let schema = self
            .statements_schema()
            .await?
            .context("pg_stat_statements is not installed.")?;
        self.client
            .batch_execute(&format!(
                "SELECT {}.pg_stat_statements_reset()",
                quote_ident(&schema)
            ))
            .await?;
        Ok(())
    }

    /// Client sessions on the server, longest-running queries first.
    pub async fn activity(&self) -> Result<Vec<SessionActivity>> {
        let rows = self
            .client
            .query(
                "SELECT pid, COALESCE(usename::text, ''), COALESCE(datname::text, ''), \
                 COALESCE(application_name, ''), COALESCE(client_addr::text, 'local'), \
                 COALESCE(state, ''), COALESCE(wait_event_type || ': ' || wait_event, ''), \
                 EXTRACT(epoch FROM now() - query_start)::float8, \
                 EXTRACT(epoch FROM now() - xact_start)::float8, COALESCE(query, ''), \
                 pid = pg_catalog.pg_backend_pid() OR application_name LIKE 'Postern%' \
                 FROM pg_catalog.pg_stat_activity WHERE backend_type = 'client backend' \
                 ORDER BY state = 'active' DESC, query_start NULLS LAST",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| SessionActivity {
                pid: r.get(0),
                user: r.get(1),
                database: r.get(2),
                application: r.get(3),
                client: r.get(4),
                state: r.get(5),
                wait: r.get(6),
                query_seconds: r.get(7),
                transaction_seconds: r.get(8),
                query: r.get(9),
                own: r.get(10),
            })
            .collect())
    }

    /// Cancels the running query of a session, or terminates the session.
    pub async fn signal_backend(&self, pid: i32, terminate: bool) -> Result<bool> {
        let sql = if terminate {
            "SELECT pg_catalog.pg_terminate_backend($1)"
        } else {
            "SELECT pg_catalog.pg_cancel_backend($1)"
        };
        Ok(self.client.query_one(sql, &[&pid]).await?.get(0))
    }

    pub async fn roles(&self) -> Result<Vec<RoleInfo>> {
        let rows = self
            .client
            .query(
                "SELECT r.rolname::text, r.rolsuper, r.rolcanlogin, r.rolcreatedb, r.rolcreaterole, \
                 r.rolreplication, r.rolbypassrls, r.rolconnlimit, to_char(r.rolvaliduntil, 'YYYY-MM-DD HH24:MI TZ'), \
                 ARRAY(SELECT b.rolname::text FROM pg_catalog.pg_auth_members m \
                       JOIN pg_catalog.pg_roles b ON b.oid = m.roleid WHERE m.member = r.oid ORDER BY 1) \
                 FROM pg_catalog.pg_roles r WHERE r.rolname NOT LIKE 'pg\\_%' ORDER BY 1",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| RoleInfo {
                name: r.get(0),
                superuser: r.get(1),
                login: r.get(2),
                create_database: r.get(3),
                create_role: r.get(4),
                replication: r.get(5),
                bypass_rls: r.get(6),
                connection_limit: r.get(7),
                valid_until: r.get(8),
                member_of: r.get(9),
            })
            .collect())
    }

    /// Column names of every user table and view, for SQL completion.
    pub async fn completion_columns(
        &self,
    ) -> Result<std::collections::HashMap<(String, String), Vec<String>>> {
        let rows = self
            .client
            .query(
                "SELECT n.nspname, c.relname, a.attname FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r','p','v','m','f') AND a.attnum > 0 AND NOT a.attisdropped \
                 AND n.nspname NOT IN ('pg_catalog','information_schema') \
                 AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp_%' \
                 ORDER BY n.nspname, c.relname, a.attnum LIMIT 200000",
                &[],
            )
            .await?;
        let mut columns: std::collections::HashMap<(String, String), Vec<String>> =
            std::collections::HashMap::new();
        for row in rows {
            columns
                .entry((row.get(0), row.get(1)))
                .or_default()
                .push(row.get(2));
        }
        Ok(columns)
    }

    /// User-defined functions and procedures, excluding those owned by extensions.
    pub async fn routines(&self) -> Result<Vec<Routine>> {
        let rows = self
            .client
            .query(
                "SELECT p.oid, n.nspname, p.proname, pg_catalog.pg_get_function_identity_arguments(p.oid), \
                 COALESCE(pg_catalog.pg_get_function_result(p.oid), ''), p.prokind = 'p' \
                 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                 WHERE p.prokind IN ('f', 'p') AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
                 AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp_%' \
                 AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d WHERE d.classid = 'pg_catalog.pg_proc'::regclass \
                                 AND d.objid = p.oid AND d.deptype = 'e') \
                 ORDER BY n.nspname, p.proname, 4",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| Routine {
                oid: r.get(0),
                schema: r.get(1),
                name: r.get(2),
                arguments: r.get(3),
                result: r.get(4),
                procedure: r.get(5),
            })
            .collect())
    }

    pub async fn routine_definition(&self, oid: u32) -> Result<String> {
        let definition: String = self
            .client
            .query_one("SELECT pg_catalog.pg_get_functiondef($1)", &[&oid])
            .await?
            .get(0);
        Ok(format!("{};\n", definition.trim_end()))
    }

    /// Runs DDL or maintenance statements. Transactional scripts commit together;
    /// otherwise each statement runs on its own, which VACUUM requires.
    pub async fn execute(&mut self, statements: &[String], transactional: bool) -> Result<()> {
        if transactional {
            let transaction = self.client.transaction().await?;
            for statement in statements {
                transaction.batch_execute(statement).await?;
            }
            transaction.commit().await?;
        } else {
            for statement in statements {
                self.client.batch_execute(statement).await?;
            }
        }
        Ok(())
    }

    /// Current column metadata, used to check that loaded rows still match.
    pub async fn columns(&self, relation: &Relation) -> Result<Vec<Column>> {
        let name = relation.qualified_name();
        let rows = self.client.query(
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
             pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
             EXISTS (SELECT 1 FROM pg_catalog.pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY(i.indkey)), \
             (a.attgenerated <> '' OR a.attidentity = 'a'), pg_catalog.col_description(a.attrelid, a.attnum), \
             pg_catalog.quote_ident(tn.nspname) || '.' || pg_catalog.quote_ident(t.typname), \
             pg_catalog.format_type(COALESCE(NULLIF(et.typbasetype, 0), et.oid), NULL) NOT IN \
               ('json', 'xml', 'point', 'line', 'lseg', 'box', 'path', 'polygon', 'circle', \
                'jsonpath', 'xid', 'cid', 'aclitem', 'pg_snapshot', 'txid_snapshot', 'int2vector') \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
             JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace \
             JOIN pg_catalog.pg_type et ON et.oid = CASE WHEN t.typcategory = 'A' AND t.typelem <> 0 THEN t.typelem ELSE t.oid END \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum", &[&name]).await?;
        Ok(rows
            .iter()
            .map(|r| Column {
                name: r.get(0),
                data_type: r.get(1),
                nullable: r.get(2),
                default: r.get(3),
                primary_key: r.get(4),
                generated: r.get(5),
                comment: r.get(6),
                cast_type: r.get(7),
                sortable: r.get(8),
            })
            .collect())
    }

    pub async fn structure(&self, relation: &Relation) -> Result<TableStructure> {
        let name = relation.qualified_name();
        let columns = self.columns(relation).await?;
        let rows = self.client.query(
            "SELECT conname, pg_catalog.pg_get_constraintdef(oid, true) FROM pg_catalog.pg_constraint \
             WHERE conrelid = $1::text::regclass AND contype <> 'n' ORDER BY conname", &[&name]).await?;
        let constraints: Vec<(String, String)> =
            rows.iter().map(|r| (r.get(0), r.get(1))).collect();
        let rows = self
            .client
            .query(
                "SELECT ci.relname, pg_catalog.pg_get_indexdef(i.indexrelid), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c WHERE c.conindid = i.indexrelid \
                         AND c.conrelid = i.indrelid AND c.contype IN ('p', 'u', 'x')), \
                 pg_catalog.pg_relation_size(i.indexrelid), s.idx_scan \
                 FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ci ON ci.oid = i.indexrelid \
                 LEFT JOIN pg_catalog.pg_stat_all_indexes s ON s.indexrelid = i.indexrelid \
                 WHERE i.indrelid = $1::text::regclass ORDER BY ci.relname",
                &[&name],
            )
            .await?;
        let indexes: Vec<IndexInfo> = rows
            .iter()
            .map(|r| IndexInfo {
                name: r.get(0),
                definition: r.get(1),
                constraint: r.get(2),
                bytes: r.get(3),
                scans: r.get(4),
            })
            .collect();
        let foreign_keys = self.foreign_keys("c.conrelid", &name).await?;
        let referenced_by = self.foreign_keys("c.confrelid", &name).await?;
        let comment = self
            .client
            .query_one(
                "SELECT pg_catalog.obj_description($1::text::regclass, 'pg_class')",
                &[&name],
            )
            .await?
            .get(0);
        let stats = if matches!(relation.kind.as_str(), "r" | "p" | "m") {
            let row = self
                .client
                .query_one(
                    "SELECT pg_catalog.pg_total_relation_size(c.oid), pg_catalog.pg_relation_size(c.oid), \
                     pg_catalog.pg_indexes_size(c.oid), s.n_live_tup, s.n_dead_tup, s.seq_scan, s.idx_scan, \
                     to_char(GREATEST(s.last_vacuum, s.last_autovacuum), 'YYYY-MM-DD HH24:MI:SS TZ'), \
                     to_char(GREATEST(s.last_analyze, s.last_autoanalyze), 'YYYY-MM-DD HH24:MI:SS TZ') \
                     FROM pg_catalog.pg_class c LEFT JOIN pg_catalog.pg_stat_all_tables s ON s.relid = c.oid \
                     WHERE c.oid = $1::text::regclass",
                    &[&name],
                )
                .await?;
            Some(TableStats {
                total_bytes: row.get(0),
                table_bytes: row.get(1),
                index_bytes: row.get(2),
                live_rows: row.get(3),
                dead_rows: row.get(4),
                sequential_scans: row.get(5),
                index_scans: row.get(6),
                last_vacuum: row.get(7),
                last_analyze: row.get(8),
            })
        } else {
            None
        };
        let privileges = self
            .client
            .query(
                "SELECT grantee, string_agg(privilege_type, ', ' ORDER BY privilege_type) \
                 FROM information_schema.role_table_grants \
                 WHERE table_schema = $1 AND table_name = $2 GROUP BY grantee ORDER BY grantee",
                &[&relation.schema, &relation.name],
            )
            .await?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        let mut structure = TableStructure {
            columns,
            constraints,
            indexes,
            stats,
            definition: String::new(),
            comment,
            foreign_keys,
            referenced_by,
            privileges,
        };
        structure.definition = self.definition(relation, &structure).await?;
        Ok(structure)
    }

    /// Generates DDL that recreates the relation: columns with identity, generated
    /// expressions, collations and defaults; partitioning and inheritance; storage
    /// options; constraints; indexes; triggers; row-level security; and comments.
    /// Ownership and privileges are not included.
    async fn definition(&self, relation: &Relation, structure: &TableStructure) -> Result<String> {
        let name = relation.qualified_name();
        let mut out = String::from(
            "-- Generated by Postern. Review before running; ownership and privileges are not included.\n",
        );
        let comments = |out: &mut String| {
            if let Some(comment) = &structure.comment {
                out.push_str(&format!(
                    "COMMENT ON {} {name} IS {};\n",
                    relation.ddl_keyword(),
                    sql::quote_literal(comment)
                ));
            }
            for column in &structure.columns {
                if let Some(comment) = &column.comment {
                    out.push_str(&format!(
                        "COMMENT ON COLUMN {name}.{} IS {};\n",
                        quote_ident(&column.name),
                        sql::quote_literal(comment)
                    ));
                }
            }
        };
        if matches!(relation.kind.as_str(), "v" | "m") {
            let definition: String = self
                .client
                .query_one(
                    "SELECT pg_catalog.pg_get_viewdef($1::text::regclass, true)",
                    &[&name],
                )
                .await?
                .get(0);
            let definition = definition.trim_end().trim_end_matches(';');
            if relation.kind == "m" {
                out.push_str(&format!(
                    "CREATE MATERIALIZED VIEW {name} AS\n{definition}\nWITH DATA;\n"
                ));
                for index in &structure.indexes {
                    out.push_str(&format!("{};\n", index.definition));
                }
            } else {
                out.push_str(&format!(
                    "CREATE OR REPLACE VIEW {name} AS\n{definition};\n"
                ));
            }
            comments(&mut out);
            return Ok(out);
        }
        let table = self
            .client
            .query_one(
                "SELECT c.relpersistence::text, c.relispartition, \
                 CASE WHEN c.relkind = 'p' THEN pg_catalog.pg_get_partkeydef(c.oid) END, \
                 pg_catalog.pg_get_expr(c.relpartbound, c.oid), \
                 (SELECT pg_catalog.quote_ident(pn.nspname) || '.' || pg_catalog.quote_ident(pc.relname) \
                  FROM pg_catalog.pg_inherits i JOIN pg_catalog.pg_class pc ON pc.oid = i.inhparent \
                  JOIN pg_catalog.pg_namespace pn ON pn.oid = pc.relnamespace \
                  WHERE i.inhrelid = c.oid AND c.relispartition), \
                 (SELECT string_agg(pg_catalog.quote_ident(pn.nspname) || '.' || pg_catalog.quote_ident(pc.relname), ', ' ORDER BY i.inhseqno) \
                  FROM pg_catalog.pg_inherits i JOIN pg_catalog.pg_class pc ON pc.oid = i.inhparent \
                  JOIN pg_catalog.pg_namespace pn ON pn.oid = pc.relnamespace \
                  WHERE i.inhrelid = c.oid AND NOT c.relispartition), \
                 array_to_string(c.reloptions, ', '), c.relrowsecurity, c.relforcerowsecurity, \
                 (SELECT pg_catalog.quote_ident(s.srvname) FROM pg_catalog.pg_foreign_table f \
                  JOIN pg_catalog.pg_foreign_server s ON s.oid = f.ftserver WHERE f.ftrelid = c.oid), \
                 (SELECT string_agg(pg_catalog.quote_ident(split_part(o, '=', 1)) || ' ' || \
                         pg_catalog.quote_literal(substr(o, strpos(o, '=') + 1)), ', ') \
                  FROM pg_catalog.pg_foreign_table f, unnest(f.ftoptions) o WHERE f.ftrelid = c.oid) \
                 FROM pg_catalog.pg_class c WHERE c.oid = $1::text::regclass",
                &[&name],
            )
            .await?;
        let persistence: String = table.get(0);
        let is_partition: bool = table.get(1);
        let partition_key: Option<String> = table.get(2);
        let bound: Option<String> = table.get(3);
        let parent: Option<String> = table.get(4);
        let inherits: Option<String> = table.get(5);
        let options: Option<String> = table.get(6);
        let row_security: bool = table.get(7);
        let force_row_security: bool = table.get(8);
        let server: Option<String> = table.get(9);
        let server_options: Option<String> = table.get(10);
        let rows = self
            .client
            .query(
                "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, \
                 pg_catalog.pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text, \
                 CASE WHEN a.attcollation <> t.typcollation AND a.attcollation <> 0 THEN \
                   pg_catalog.quote_ident(cn.nspname) || '.' || pg_catalog.quote_ident(co.collname) END, \
                 a.attislocal, pg_catalog.pg_get_serial_sequence($1, a.attname) \
                 FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
                 LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 LEFT JOIN pg_catalog.pg_collation co ON co.oid = a.attcollation \
                 LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid = co.collnamespace \
                 WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
                &[&name],
            )
            .await?;
        let mut sequences = Vec::new();
        let mut fields = Vec::new();
        for row in &rows {
            let column: String = row.get(0);
            let data_type: String = row.get(1);
            let not_null: bool = row.get(2);
            let default: Option<String> = row.get(3);
            let identity: String = row.get(4);
            let generated: String = row.get(5);
            let collation: Option<String> = row.get(6);
            let local: bool = row.get(7);
            let sequence: Option<String> = row.get(8);
            if let Some(sequence) = sequence.filter(|_| identity.is_empty()) {
                sequences.push((sequence, column.clone()));
            }
            if !local {
                continue;
            }
            let mut field = format!("    {} {data_type}", quote_ident(&column));
            if let Some(collation) = collation {
                field.push_str(&format!(" COLLATE {collation}"));
            }
            match (identity.as_str(), generated.as_str(), &default) {
                ("a", _, _) => field.push_str(" GENERATED ALWAYS AS IDENTITY"),
                ("d", _, _) => field.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
                (_, "s", Some(expression)) => {
                    field.push_str(&format!(" GENERATED ALWAYS AS ({expression}) STORED"))
                }
                (_, "v", Some(expression)) => {
                    field.push_str(&format!(" GENERATED ALWAYS AS ({expression}) VIRTUAL"))
                }
                (_, _, Some(expression)) => field.push_str(&format!(" DEFAULT {expression}")),
                _ => {}
            }
            if not_null {
                field.push_str(" NOT NULL");
            }
            fields.push(field);
        }
        for (sequence, _) in &sequences {
            out.push_str(&format!("CREATE SEQUENCE IF NOT EXISTS {sequence};\n"));
        }
        let unlogged = if persistence == "u" { "UNLOGGED " } else { "" };
        let keyword = if server.is_some() {
            "FOREIGN TABLE"
        } else {
            "TABLE"
        };
        match (is_partition, &parent, &bound) {
            (true, Some(parent), Some(bound)) => {
                out.push_str(&format!(
                    "CREATE {unlogged}TABLE {name} PARTITION OF {parent} {bound}"
                ));
            }
            _ => {
                out.push_str(&format!(
                    "CREATE {unlogged}{keyword} {name} (\n{}\n)",
                    fields.join(",\n")
                ));
                if let Some(inherits) = &inherits {
                    out.push_str(&format!(" INHERITS ({inherits})"));
                }
            }
        }
        if let Some(key) = partition_key {
            out.push_str(&format!(" PARTITION BY {key}"));
        }
        if let Some(server) = server {
            out.push_str(&format!(" SERVER {server}"));
            if let Some(options) = server_options {
                out.push_str(&format!(" OPTIONS ({options})"));
            }
        }
        if let Some(options) = options {
            out.push_str(&format!(" WITH ({options})"));
        }
        out.push_str(";\n");
        for (sequence, column) in &sequences {
            out.push_str(&format!(
                "ALTER SEQUENCE {sequence} OWNED BY {name}.{};\n",
                quote_ident(column)
            ));
        }
        let rows = self
            .client
            .query(
                "SELECT conname, pg_catalog.pg_get_constraintdef(oid, true) FROM pg_catalog.pg_constraint \
                 WHERE conrelid = $1::text::regclass AND contype <> 'n' AND conislocal \
                 ORDER BY contype = 'f', conname",
                &[&name],
            )
            .await?;
        for row in &rows {
            let constraint: String = row.get(0);
            let definition: String = row.get(1);
            out.push_str(&format!(
                "ALTER TABLE ONLY {name} ADD CONSTRAINT {} {definition};\n",
                quote_ident(&constraint)
            ));
        }
        for index in structure.indexes.iter().filter(|i| !i.constraint) {
            out.push_str(&format!("{};\n", index.definition));
        }
        let rows = self
            .client
            .query(
                "SELECT pg_catalog.pg_get_triggerdef(oid, true) FROM pg_catalog.pg_trigger \
                 WHERE tgrelid = $1::text::regclass AND NOT tgisinternal ORDER BY tgname",
                &[&name],
            )
            .await?;
        for row in &rows {
            out.push_str(&format!("{};\n", row.get::<_, String>(0)));
        }
        if row_security {
            out.push_str(&format!("ALTER TABLE {name} ENABLE ROW LEVEL SECURITY;\n"));
        }
        if force_row_security {
            out.push_str(&format!("ALTER TABLE {name} FORCE ROW LEVEL SECURITY;\n"));
        }
        let rows = self
            .client
            .query(
                "SELECT policyname, permissive, cmd, array_to_string(roles, ', '), qual, with_check \
                 FROM pg_catalog.pg_policies WHERE schemaname = $1 AND tablename = $2 ORDER BY policyname",
                &[&relation.schema, &relation.name],
            )
            .await?;
        for row in &rows {
            let policy: String = row.get(0);
            let permissive: String = row.get(1);
            let command: String = row.get(2);
            let roles: String = row.get(3);
            let using: Option<String> = row.get(4);
            let check: Option<String> = row.get(5);
            out.push_str(&format!(
                "CREATE POLICY {} ON {name} AS {permissive} FOR {command} TO {roles}{}{};\n",
                quote_ident(&policy),
                using.map(|u| format!(" USING ({u})")).unwrap_or_default(),
                check
                    .map(|c| format!(" WITH CHECK ({c})"))
                    .unwrap_or_default()
            ));
        }
        comments(&mut out);
        Ok(out)
    }

    /// Foreign keys where `side` (`c.conrelid` or `c.confrelid`) is the relation.
    async fn foreign_keys(&self, side: &str, relation: &str) -> Result<Vec<ForeignKey>> {
        let sql = format!(
            "SELECT c.conname, n.nspname, t.relname, \
             ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY k(attnum, ord) \
                   JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum ORDER BY k.ord), \
             fn.nspname, ft.relname, \
             ARRAY(SELECT a.attname::text FROM unnest(c.confkey) WITH ORDINALITY k(attnum, ord) \
                   JOIN pg_catalog.pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum ORDER BY k.ord) \
             FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_class ft ON ft.oid = c.confrelid JOIN pg_catalog.pg_namespace fn ON fn.oid = ft.relnamespace \
             WHERE c.contype = 'f' AND {side} = $1::text::regclass AND c.conparentid = 0 \
             ORDER BY n.nspname, t.relname, c.conname"
        );
        let rows = self.client.query(&sql, &[&relation]).await?;
        Ok(rows
            .iter()
            .map(|r| ForeignKey {
                name: r.get(0),
                schema: r.get(1),
                table: r.get(2),
                columns: r.get(3),
                referenced_schema: r.get(4),
                referenced_table: r.get(5),
                referenced_columns: r.get(6),
            })
            .collect())
    }

    pub async fn browse(&self, request: &BrowseRequest) -> Result<TablePage> {
        let start = Instant::now();
        let (sql, params) = browse_sql(request)?;
        let stream = self.query_text(&sql, &params).await?;
        pin_mut!(stream);
        let mut rows = Vec::new();
        let mut bytes: usize = 0;
        while let Some(row) = stream.next().await {
            let row = row?;
            let values: Vec<Option<String>> = (0..row.len())
                .map(|i| row.try_get(i))
                .collect::<Result<_, _>>()?;
            bytes += values.iter().flatten().map(String::len).sum::<usize>();
            ensure!(
                bytes <= MAX_RESULT_BYTES,
                "This page exceeds 16 MiB. Reduce the page size or select fewer columns in SQL."
            );
            rows.push(values);
        }
        let has_more = rows.len() > request.page_size;
        rows.truncate(request.page_size);
        Ok(TablePage {
            result: QueryResult {
                columns: request.columns.iter().map(|c| c.name.clone()).collect(),
                affected: rows.len() as u64,
                rows,
                truncated: false,
                command: "SELECT".into(),
                sql: String::new(),
            },
            has_more,
            elapsed_ms: start.elapsed().as_millis(),
        })
    }

    /// Candidate rows for a foreign key: the key value and a JSON preview of the
    /// row. An empty search lists the first keys in order; otherwise rows whose key
    /// starts with, or whose JSON contains, the search text.
    pub async fn lookup_keys(
        &self,
        relation: &Relation,
        column: &str,
        search: &str,
    ) -> Result<Vec<(String, String)>> {
        let key = format!("t.{}", quote_ident(column));
        let table = relation.qualified_name();
        let rows = if search.trim().is_empty() {
            self.client
                .query(
                    &format!(
                        "SELECT {key}::text, left(row_to_json(t)::text, 300) FROM {table} AS t \
                         WHERE {key} IS NOT NULL ORDER BY {key} LIMIT 100"
                    ),
                    &[],
                )
                .await?
        } else {
            self.client
                .query(
                    &format!(
                        "SELECT {key}::text, left(row_to_json(t)::text, 300) FROM {table} AS t \
                         WHERE {key} IS NOT NULL AND (starts_with(lower({key}::text), lower($1)) \
                         OR strpos(lower(row_to_json(t)::text), lower($1)) > 0) LIMIT 100"
                    ),
                    &[&search.trim()],
                )
                .await?
        };
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }

    /// Exact number of rows matching the request's filter.
    pub async fn count(&self, request: &BrowseRequest) -> Result<i64> {
        let mut params = Vec::new();
        let predicate = sql::filter_predicate(&request.filter, &request.columns, &mut params)?;
        let sql = format!(
            "SELECT count(*) FROM {} AS _pgm_row{}",
            request.relation.qualified_name(),
            predicate.map(|p| format!(" WHERE {p}")).unwrap_or_default()
        );
        let stream = self.query_text(&sql, &params).await?;
        pin_mut!(stream);
        let row = stream.next().await.context("COUNT returned no row.")??;
        Ok(row.get(0))
    }

    /// Runs a query whose parameters are all text in one round trip. Declaring
    /// the parameter types skips the separate prepare step.
    async fn query_text(
        &self,
        sql: &str,
        params: &[String],
    ) -> Result<tokio_postgres::RowStream, tokio_postgres::Error> {
        self.client
            .query_typed_raw(
                sql,
                params
                    .iter()
                    .map(|p| (p as &(dyn ToSql + Sync), tokio_postgres::types::Type::TEXT)),
            )
            .await
    }

    /// Writes every row matching the request's filter and sort to a CSV file,
    /// streaming rows so the size is not limited by memory or the display cap.
    pub async fn export_table_csv(
        &self,
        request: &BrowseRequest,
        path: &std::path::Path,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<u64> {
        use std::io::Write;
        let (sql, params) = sql::select_sql(request)?;
        let mut file = crate::services::storage::AtomicFile::create_user(path)?;
        file.write_all(
            sql::csv_line(request.columns.iter().map(|c| Some(c.name.as_str()))).as_bytes(),
        )?;
        let stream = self.query_text(&sql, &params).await?;
        pin_mut!(stream);
        let mut count = 0u64;
        while let Some(row) = stream.next().await {
            let row = row?;
            let values: Vec<Option<&str>> = (0..row.len())
                .map(|i| row.try_get(i))
                .collect::<Result<_, _>>()?;
            file.write_all(sql::csv_line(values.into_iter()).as_bytes())?;
            count += 1;
            if count.is_multiple_of(50_000) {
                progress(count);
            }
        }
        file.commit()?;
        Ok(count)
    }

    /// Runs a read-only query through `COPY (...) TO STDOUT` and writes the CSV
    /// output to a file. The query is executed again.
    pub async fn export_query_csv(
        &self,
        query: &str,
        path: &std::path::Path,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<u64> {
        use std::io::Write;
        let statements = lexer::split_statements(query);
        ensure!(
            statements.len() == 1,
            "Select exactly one statement to export its complete result."
        );
        let text = &query[statements[0].range.clone()];
        ensure!(
            is_read_only_query(text),
            "Only SELECT, VALUES, TABLE, and read-only WITH queries can be exported in full."
        );
        let mut file = crate::services::storage::AtomicFile::create_user(path)?;
        let stream = self
            .client
            .copy_out(&format!(
                "COPY ({text}) TO STDOUT WITH (FORMAT csv, HEADER true)"
            ))
            .await?;
        pin_mut!(stream);
        let mut bytes = 0u64;
        let mut lines = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            lines += chunk.iter().filter(|b| **b == b'\n').count() as u64;
            bytes += chunk.len() as u64;
            file.write_all(&chunk)?;
            if bytes % (8 * 1024 * 1024) < chunk.len() as u64 {
                progress(lines.saturating_sub(1));
            }
        }
        file.commit()?;
        Ok(lines.saturating_sub(1))
    }

    /// Loads a CSV file into a table with `COPY ... FROM STDIN` in one transaction.
    /// Any malformed row or constraint violation rolls back the whole import.
    pub async fn import_csv(
        &mut self,
        relation: &Relation,
        columns: &[String],
        path: &std::path::Path,
        header: bool,
        delimiter: char,
    ) -> Result<u64> {
        use futures_util::SinkExt;
        use std::io::Read;
        ensure!(
            relation.is_table(),
            "CSV can only be imported into a base table."
        );
        ensure!(!columns.is_empty(), "Choose at least one column to import.");
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("Cannot open {}.", path.display()))?;
        let transaction = self.client.transaction().await?;
        let sink = transaction
            .copy_in(&sql::copy_from_csv_sql(
                relation, columns, header, delimiter,
            ))
            .await?;
        pin_mut!(sink);
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            sink.send(std::io::Cursor::new(buffer[..read].to_vec()))
                .await?;
        }
        let rows = sink.finish().await?;
        transaction.commit().await?;
        Ok(rows)
    }

    /// Executes a script one statement at a time, like psql. Each statement commits
    /// on its own unless the script opens a transaction. Execution stops at the
    /// first error. Results are capped for display; statements always run in full.
    pub async fn query(&self, script: &str) -> Result<QueryOutput> {
        let start = Instant::now();
        let statements = lexer::split_statements(script);
        let mut output = QueryOutput {
            statements: statements.len(),
            ..Default::default()
        };
        self.take_notices();
        let mut bytes: usize = 0;
        let mut total_rows = 0;
        let mut dropped_results = false;
        for (index, statement) in statements.iter().enumerate() {
            let text = &script[statement.range.clone()];
            let statement_offset = lexer::byte_to_char(script, statement.range.start);
            if let Some(message) = unsupported_statement(text) {
                output.error = Some(message.into());
                output.error_offset = Some(statement_offset);
                break;
            }
            let command = lexer::first_word(text).unwrap_or_default();
            let fresh = || QueryResult {
                command: command.clone(),
                sql: text.to_owned(),
                ..Default::default()
            };
            let mut current = fresh();
            let mut failure = None;
            match self.client.simple_query_raw(text).await {
                Ok(stream) => {
                    pin_mut!(stream);
                    while let Some(message) = stream.next().await {
                        match message {
                            Ok(SimpleQueryMessage::RowDescription(columns)) => {
                                current.columns =
                                    columns.iter().map(|c| c.name().to_owned()).collect();
                            }
                            Ok(SimpleQueryMessage::Row(row)) => {
                                let size = (0..row.len())
                                    .filter_map(|i| row.get(i))
                                    .map(str::len)
                                    .sum::<usize>();
                                if total_rows < MAX_RESULT_ROWS
                                    && bytes.saturating_add(size) <= MAX_RESULT_BYTES
                                    && output.results.len() < MAX_RESULTS
                                {
                                    current.rows.push(
                                        (0..row.len())
                                            .map(|i| row.get(i).map(str::to_owned))
                                            .collect(),
                                    );
                                    total_rows += 1;
                                    bytes += size;
                                } else {
                                    current.truncated = true;
                                }
                            }
                            Ok(SimpleQueryMessage::CommandComplete(count)) => {
                                current.affected = count;
                                let finished = std::mem::replace(&mut current, fresh());
                                if output.results.len() < MAX_RESULTS {
                                    output.results.push(finished);
                                } else {
                                    dropped_results = true;
                                }
                            }
                            Err(error) => {
                                if !current.columns.is_empty() && output.results.len() < MAX_RESULTS
                                {
                                    current.truncated = true;
                                    output.results.push(std::mem::take(&mut current));
                                }
                                failure = Some(error);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(error) => failure = Some(error),
            }
            output.messages.extend(self.take_notices());
            if let Some(error) = failure {
                let position =
                    error
                        .as_db_error()
                        .and_then(|db| db.position())
                        .and_then(|position| match position {
                            tokio_postgres::error::ErrorPosition::Original(position) => {
                                Some(*position as usize)
                            }
                            _ => None,
                        });
                let offset = statement_offset + position.map_or(0, |p| p.saturating_sub(1));
                let mut message = error_message(&error);
                if statements.len() > 1 {
                    message += &format!(
                        "\nStatement {} of {} failed; {} earlier {} completed.",
                        index + 1,
                        statements.len(),
                        index,
                        if index == 1 {
                            "statement"
                        } else {
                            "statements"
                        }
                    );
                }
                output.error = Some(message);
                output.error_offset = Some(offset);
                break;
            }
            output.completed += 1;
        }
        if dropped_results {
            output.messages.push(format!(
                "Only the first {MAX_RESULTS} statement results are displayed. Remaining statements still executed."
            ));
        }
        output.elapsed_ms = start.elapsed().as_millis();
        Ok(output)
    }

    pub async fn update_cell(
        &mut self,
        target: &MutationTarget,
        original: &[Option<String>],
        column_index: usize,
        value: Option<String>,
    ) -> Result<()> {
        self.update_row(target, original, &[(column_index, value)])
            .await
    }

    pub async fn update_row(
        &mut self,
        target: &MutationTarget,
        original: &[Option<String>],
        changes: &[(usize, Option<String>)],
    ) -> Result<()> {
        let target = self.current_target(target).await?;
        let mutation = sql::update_row_sql(&target, original, changes)?;
        self.apply(&[mutation]).await
    }

    pub async fn delete_row(
        &mut self,
        target: &MutationTarget,
        original: &[Option<String>],
    ) -> Result<()> {
        self.delete_rows(target, &[original.to_vec()]).await
    }

    pub async fn delete_rows(
        &mut self,
        target: &MutationTarget,
        originals: &[Vec<Option<String>>],
    ) -> Result<()> {
        ensure!(!originals.is_empty(), "Select at least one row to delete.");
        let target = self.current_target(target).await?;
        let mutation = sql::delete_rows_sql(&target, originals)?;
        self.apply(&[mutation]).await
    }

    pub async fn insert_row(
        &mut self,
        target: &MutationTarget,
        values: &[InsertValue],
    ) -> Result<()> {
        self.insert_rows(target, &[values.to_vec()]).await
    }

    pub async fn insert_rows(
        &mut self,
        target: &MutationTarget,
        rows: &[Vec<InsertValue>],
    ) -> Result<()> {
        ensure!(!rows.is_empty(), "There are no rows to insert.");
        let target = self.current_target(target).await?;
        let mutations = rows
            .iter()
            .map(|values| sql::insert_row_sql(&target, values))
            .collect::<Result<Vec<_>>>()?;
        self.apply(&mutations).await
    }

    /// Applies staged grid changes in one transaction: updates, then inserts,
    /// then deletes. Any conflict or error rolls back every change.
    pub async fn apply_changes(
        &mut self,
        target: &MutationTarget,
        changes: &RowChanges,
    ) -> Result<()> {
        ensure!(!changes.is_empty(), "There are no changes to save.");
        let target = self.current_target(target).await?;
        let mut mutations = Vec::new();
        for (original, values) in &changes.updates {
            mutations.push(sql::update_row_sql(&target, original, values)?);
        }
        for values in &changes.inserts {
            mutations.push(sql::insert_row_sql(&target, values)?);
        }
        if !changes.deletes.is_empty() {
            mutations.push(sql::delete_rows_sql(&target, &changes.deletes)?);
        }
        self.apply(&mutations).await
    }

    /// Returns the target with fresh metadata after checking that the columns the
    /// rows were loaded with still match the table.
    async fn current_target(&self, target: &MutationTarget) -> Result<MutationTarget> {
        ensure!(
            target.relation.is_table(),
            "Only base tables can be edited."
        );
        let structure = self.structure(&target.relation).await?;
        ensure!(
            structure.columns == target.columns,
            "Table structure changed. Refresh before saving."
        );
        Ok(MutationTarget {
            relation: target.relation.clone(),
            columns: structure.columns,
        })
    }

    async fn apply(&mut self, mutations: &[Mutation]) -> Result<()> {
        let transaction = self.client.transaction().await?;
        for mutation in mutations {
            let params: Vec<&(dyn ToSql + Sync)> = mutation
                .params
                .iter()
                .map(|p| p as &(dyn ToSql + Sync))
                .collect();
            let affected = transaction.execute(&mutation.sql, &params).await?;
            if affected != mutation.expected {
                transaction.rollback().await?;
                bail!(
                    "No change saved: one or more rows changed or disappeared since they were loaded. Refresh and try again."
                );
            }
        }
        transaction.commit().await?;
        Ok(())
    }
}

/// Statements the simple query protocol cannot run without special handling.
fn unsupported_statement(text: &str) -> Option<&'static str> {
    if text.trim_start().starts_with('\\') {
        return Some("psql meta-commands such as \\d are not supported. Write SQL instead.");
    }
    if lexer::first_word(text).as_deref() == Some("COPY")
        && (lexer::contains_word(text, "stdin") || lexer::contains_word(text, "stdout"))
    {
        return Some(
            "COPY ... FROM STDIN and COPY ... TO STDOUT are not supported in the SQL editor. Use Import CSV or Export CSV in the table view.",
        );
    }
    None
}

/// True for statements that only read data: SELECT, VALUES, TABLE, SHOW, and WITH
/// queries without data-modifying words.
pub fn is_read_only_query(text: &str) -> bool {
    match lexer::first_word(text).as_deref() {
        Some("SELECT" | "VALUES" | "TABLE") => !lexer::contains_word(text, "into"),
        Some("WITH") => !["insert", "update", "delete", "merge", "into"]
            .iter()
            .any(|word| lexer::contains_word(text, word)),
        _ => false,
    }
}

/// One-based line and column of a character offset.
pub fn line_and_column(text: &str, char_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for c in text.chars().take(char_offset) {
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

pub fn error_message(error: &tokio_postgres::Error) -> String {
    if let Some(db) = error.as_db_error() {
        let mut text = format!("{} (SQLSTATE {})", db.message(), db.code().code());
        if let Some(detail) = db.detail() {
            text += &format!("\n{detail}");
        }
        if let Some(hint) = db.hint() {
            text += &format!("\nHint: {hint}");
        }
        text
    } else {
        error.to_string()
    }
}
