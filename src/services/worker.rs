//! Asynchronous database worker.
//!
//! Commands run on lanes. The data lane serves the table browser on its own
//! session. Each SQL document has its own SQL lane and session, opened on its
//! first query, so documents have independent transactions and can run at the
//! same time. The control lane connects, switches databases, and disconnects.
//! Lanes run concurrently; each runs one command at a time, and control commands
//! wait for every session.

use crate::{
    models::*,
    services::{
        database::{Database, STATEMENTS_LIBRARY, SessionKind, error_message, tls},
        server::{self, LocalServer},
        ssh::SshTunnel,
    },
    sql,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
};
use tokio::sync::{Mutex, Notify, mpsc as async_channel};
use uuid::Uuid;

/// The SQL session opened with every connection, used by `Command::Query`.
pub const SHARED_SESSION: Uuid = Uuid::nil();

/// A hidden session that counts table rows, so counting never delays browsing.
pub const COUNT_SESSION: Uuid = Uuid::from_u128(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    Data,
    /// The SQL session of one document, or [`SHARED_SESSION`].
    Sql(Uuid),
    Control,
}

pub enum Command {
    Connect(ConnectionProfile, String),
    Test(ConnectionProfile, String),
    DiscoverDatabases(ConnectionProfile, String),
    Databases,
    SwitchDatabase(String),
    Disconnect,
    Catalog,
    Load(Relation, usize),
    /// Reloads metadata and rows while keeping the filter, sort, and page.
    Reload(BrowseRequest),
    Browse(BrowseRequest),
    /// Counts rows matching a request's filter on [`COUNT_SESSION`].
    Count(BrowseRequest),
    /// Runs SQL on the shared SQL session.
    Query(String),
    /// Runs SQL on a document's own SQL session.
    QueryIn(Uuid, String),
    /// Closes a document's SQL session, rolling back any open transaction.
    CloseSession(Uuid),
    Update(MutationTarget, Vec<Option<String>>, usize, Option<String>),
    UpdateRow(
        MutationTarget,
        Vec<Option<String>>,
        Vec<(usize, Option<String>)>,
    ),
    Insert(MutationTarget, Vec<InsertValue>),
    InsertRows(MutationTarget, Vec<Vec<InsertValue>>),
    Delete(MutationTarget, Vec<Vec<Option<String>>>),
    ApplyChanges(MutationTarget, RowChanges),
    /// Streams every row matching the request to a CSV file.
    ExportTable(BrowseRequest, std::path::PathBuf),
    /// Re-runs a read-only query on a document's SQL session and writes its CSV output.
    ExportQuery(Uuid, String, std::path::PathBuf),
    ImportCsv(CsvImport),
    Routines,
    RoutineDefinition(u32),
    /// Candidate rows for a foreign key picker: table, key column, search text.
    LookupKeys(Relation, String, String),
    Activity,
    IndexReport,
    SlowQueries(SlowQueryRequest),
    /// Creates pg_stat_statements, then reloads the slow queries.
    EnableQueryStats(SlowQueryRequest),
    /// Resets pg_stat_statements, then reloads the slow queries.
    ResetQueryStats(SlowQueryRequest),
    /// Adds pg_stat_statements to `shared_preload_libraries` with ALTER SYSTEM;
    /// it loads at the next server start.
    ConfigureQueryStats(SlowQueryRequest),
    /// Configures pg_stat_statements, restarts the local server when it does not
    /// load the library yet, reconnects, and creates the extension. Runs on the
    /// control lane because the restart closes every session.
    SetUpQueryStats(SlowQueryRequest),
    /// Cancels a session's query, or terminates the session when true.
    SignalBackend(i32, bool),
    Roles,
    /// DDL or maintenance statements with a description for the status line.
    Execute {
        statements: Vec<String>,
        transactional: bool,
        description: String,
    },
}

/// A CSV file to load into a table with COPY.
#[derive(Clone, Debug)]
pub struct CsvImport {
    pub relation: Relation,
    /// Table columns in the order of the file's fields.
    pub columns: Vec<String>,
    pub path: std::path::PathBuf,
    pub header: bool,
    pub delimiter: char,
}

impl Command {
    pub fn lane(&self) -> Lane {
        match self {
            Self::Query(_) => Lane::Sql(SHARED_SESSION),
            Self::Count(_) => Lane::Sql(COUNT_SESSION),
            Self::QueryIn(id, _) | Self::ExportQuery(id, ..) | Self::CloseSession(id) => {
                Lane::Sql(*id)
            }
            Self::Connect(..)
            | Self::Test(..)
            | Self::DiscoverDatabases(..)
            | Self::SwitchDatabase(_)
            | Self::SetUpQueryStats(_)
            | Self::Disconnect => Lane::Control,
            _ => Lane::Data,
        }
    }
}

pub enum Event {
    Connected(ConnectionProfile, Vec<Relation>),
    Tested,
    DiscoveredDatabases(ConnectionProfile, Vec<String>),
    Databases(Vec<String>),
    Disconnected,
    Catalog(Vec<Relation>),
    Loaded(Box<TableStructure>, TablePage),
    Reloaded(Box<TableStructure>, TablePage, Box<BrowseRequest>),
    Page(TablePage),
    /// The table and filter that were counted, and the number of rows.
    Counted(Relation, FilterSet, i64),
    Query(QueryOutput),
    Mutated,
    Exported(u64, std::path::PathBuf),
    Imported(u64),
    Routines(Vec<Routine>),
    /// Column names by (schema, table) for SQL completion.
    CompletionColumns(std::collections::HashMap<(String, String), Vec<String>>),
    RoutineDefinition(u32, String),
    /// Foreign key candidates for a search: the search text and (key, preview) rows.
    KeyCandidates(String, Vec<(String, String)>),
    Executed(String),
    SessionClosed,
    Activity(Vec<SessionActivity>),
    IndexReport(IndexReport),
    SlowQueries(SlowQueryReport),
    /// A cancel or terminate request: the process ID and whether it was delivered.
    Signalled(i32, bool),
    Roles(Vec<RoleInfo>),
    /// An informational message that does not finish the lane's operation.
    Notice(String),
    /// Progress of a long operation, such as an export.
    Progress(String),
    Error(String),
}

impl Event {
    /// True when the event ends the operation running on its lane.
    pub fn finishes(&self) -> bool {
        !matches!(
            self,
            Self::Notice(_) | Self::Progress(_) | Self::Routines(_) | Self::CompletionColumns(_)
        )
    }
}

/// Connection state shared by all sessions: the connected profile and the
/// password kept in memory for reconnecting, and the SSH tunnel, if any.
#[derive(Default)]
struct SessionContext {
    profile: Option<ConnectionProfile>,
    password: String,
    tunnel: Option<SshTunnel>,
}

impl SessionContext {
    /// Returns the local port of a running tunnel for the profile, starting or
    /// restarting it when needed. Profiles without SSH return `None`.
    async fn tunnel_port(&mut self, profile: &ConnectionProfile) -> Result<Option<u16>> {
        let Some(ssh) = &profile.ssh else {
            self.tunnel = None;
            return Ok(None);
        };
        if let Some(tunnel) = &mut self.tunnel
            && tunnel.serves(ssh, &profile.host, profile.port)
            && tunnel.is_alive()
        {
            return Ok(Some(tunnel.local_port));
        }
        let tunnel = SshTunnel::open(ssh, &profile.host, profile.port).await?;
        let port = tunnel.local_port;
        self.tunnel = Some(tunnel);
        Ok(Some(port))
    }
}

type Slot = Arc<Mutex<Option<Database>>>;

#[derive(Default)]
struct Shared {
    data: Mutex<Option<Database>>,
    /// SQL sessions by document. Lock this map before any session in it.
    sessions: Mutex<HashMap<Uuid, Slot>>,
    context: Mutex<SessionContext>,
    cancels: std::sync::Mutex<HashMap<Lane, Arc<Notify>>>,
}

impl Shared {
    fn cancel_signal(&self, lane: Lane) -> Arc<Notify> {
        self.cancels
            .lock()
            .expect("cancel signals")
            .entry(lane)
            .or_default()
            .clone()
    }
}

pub struct Worker {
    commands: async_channel::UnboundedSender<Command>,
    shared: Arc<Shared>,
    events: mpsc::Receiver<(Lane, Event)>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Worker {
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (commands, mut incoming) = async_channel::unbounded_channel::<Command>();
        let (outgoing, events) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let wake = Arc::new(wake);
        let state = shared.clone();
        runtime.spawn(async move {
            while let Some(command) = incoming.recv().await {
                let lane = command.lane();
                let shared = state.clone();
                let outgoing = outgoing.clone();
                let wake = wake.clone();
                tokio::spawn(async move {
                    let notify = |event: Event| {
                        let _ = outgoing.send((lane, event));
                        wake();
                    };
                    let result = match lane {
                        Lane::Control => control(command, &shared, &notify).await,
                        Lane::Data | Lane::Sql(_) => {
                            in_session(command, lane, &shared, &notify).await
                        }
                    };
                    notify(result.unwrap_or_else(|error| Event::Error(describe(&error))));
                });
            }
        });
        Ok(Self {
            commands,
            shared,
            events,
            runtime: Some(runtime),
        })
    }

    pub fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow::anyhow!("Database worker stopped."))
    }

    /// Cancels the running operation on every lane.
    pub fn cancel(&self) {
        let signals: Vec<Arc<Notify>> = self
            .shared
            .cancels
            .lock()
            .expect("cancel signals")
            .values()
            .cloned()
            .collect();
        for signal in signals {
            signal.notify_waiters();
        }
    }

    pub fn cancel_lane(&self, lane: Lane) {
        if lane != Lane::Control {
            self.shared.cancel_signal(lane).notify_waiters();
        }
    }

    /// Next event without its lane. Used where only one operation runs at a time.
    pub fn poll(&self) -> Option<Event> {
        self.poll_lane().map(|(_, event)| event)
    }

    pub fn poll_lane(&self) -> Option<(Lane, Event)> {
        self.events.try_recv().ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn describe(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<tokio_postgres::Error>()
        .map(error_message)
        .unwrap_or_else(|| format!("{error:#}"))
}

/// Runs a data or SQL command on its session. A closed session is reconnected
/// before the command, and again after a failure that closed it.
async fn in_session(
    command: Command,
    lane: Lane,
    shared: &Shared,
    notify: &(impl Fn(Event) + Sync),
) -> Result<Event> {
    let slot = match lane {
        Lane::Sql(id) => {
            let mut sessions = shared.sessions.lock().await;
            if let Command::CloseSession(_) = command {
                sessions.remove(&id);
                return Ok(Event::SessionClosed);
            }
            Some(sessions.entry(id).or_default().clone())
        }
        _ => None,
    };
    let kind = if slot.is_some() {
        SessionKind::Sql
    } else {
        SessionKind::Data
    };
    let cancel = shared.cancel_signal(lane);
    let mut session = match &slot {
        Some(slot) => slot.lock().await,
        None => shared.data.lock().await,
    };
    if session.is_none()
        && let Lane::Sql(id) = lane
        && id != SHARED_SESSION
    {
        let mut context = shared.context.lock().await;
        let profile = context.profile.clone().context("Not connected.")?;
        let port = context.tunnel_port(&profile).await?;
        let password = context.password.clone();
        drop(context);
        *session =
            Some(Database::connect_session(profile, &password, SessionKind::Sql, port).await?);
    }
    if session.as_ref().is_some_and(|db| db.client.is_closed()) {
        reconnect(&mut session, shared, kind).await?;
        notify(Event::Notice(lost_connection_message(kind).into()));
    }
    let db = session.as_mut().context("Not connected.")?;
    let token = db.client.cancel_token();
    let connector = tls(&db.profile)?;
    let cancelled = cancel.notified();
    let result = {
        let operation = execute(command, db, notify);
        tokio::pin!(operation);
        tokio::select! {
            result = &mut operation => result,
            _ = cancelled => {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    token.cancel_query(connector),
                )
                .await;
                operation.await
            }
        }
    };
    let result = match result {
        Ok(Event::Query(mut output)) => {
            let pid = session.as_ref().map(Database::backend_pid);
            if let (Some(pid), Ok(data)) = (pid, shared.data.try_lock())
                && let Some(data) = data.as_ref()
            {
                output.transaction = data
                    .session_state(pid)
                    .await
                    .unwrap_or(TransactionState::Unknown);
            }
            Ok(Event::Query(output))
        }
        other => other,
    };
    let closed = session.as_ref().is_some_and(|db| db.client.is_closed());
    if closed {
        let restored = reconnect(&mut session, shared, kind).await;
        let suffix = match restored {
            Ok(()) => lost_connection_message(kind).to_owned(),
            Err(error) => format!(
                "The connection was lost and could not be restored: {}",
                describe(&error)
            ),
        };
        return Err(match result {
            Err(error) => anyhow::anyhow!("{}\n\n{suffix}", describe(&error)),
            Ok(_) => anyhow::anyhow!(suffix),
        });
    }
    result
}

/// Refuses to change `shared_preload_libraries` when the profile is read-only,
/// the server lacks the extension, or the login is not a superuser.
fn ensure_configurable(db: &Database, facts: &PreloadFacts) -> Result<()> {
    ensure!(!db.profile.read_only, "This connection is read-only.");
    ensure!(
        facts.available,
        "This server does not include pg_stat_statements. Install the PostgreSQL contrib package on the server."
    );
    ensure!(
        facts.superuser,
        "Changing shared_preload_libraries requires a superuser."
    );
    Ok(())
}

fn lost_connection_message(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Sql => {
            "The SQL editor's connection was lost and has been re-established. Any open transaction was rolled back and session settings were reset."
        }
        SessionKind::Data => "The connection was lost and has been re-established.",
    }
}

async fn reconnect(slot: &mut Option<Database>, shared: &Shared, kind: SessionKind) -> Result<()> {
    let profile = slot
        .as_ref()
        .map(|db| db.profile.clone())
        .context("Not connected.")?;
    let mut context = shared.context.lock().await;
    let port = context.tunnel_port(&profile).await?;
    let password = context.password.clone();
    drop(context);
    *slot = Some(Database::connect_session(profile, &password, kind, port).await?);
    Ok(())
}

async fn execute(
    command: Command,
    db: &mut Database,
    notify: &(impl Fn(Event) + Sync),
) -> Result<Event> {
    let progress = |rows: u64| notify(Event::Progress(format!("Exported {rows} rows...")));
    Ok(match command {
        Command::Query(sql) | Command::QueryIn(_, sql) => Event::Query(db.query(&sql).await?),
        Command::Databases => Event::Databases(db.databases().await?),
        Command::Catalog => {
            let relations = db.relations().await?;
            notify(Event::Routines(db.routines().await.unwrap_or_default()));
            notify(Event::CompletionColumns(
                db.completion_columns().await.unwrap_or_default(),
            ));
            Event::Catalog(relations)
        }
        Command::Load(relation, page_size) => {
            let structure = db.structure(&relation).await?;
            let page = db
                .browse(&BrowseRequest {
                    relation,
                    columns: structure.columns.clone(),
                    page_size,
                    ..Default::default()
                })
                .await?;
            Event::Loaded(Box::new(structure), page)
        }
        Command::Reload(request) => {
            let structure = db.structure(&request.relation).await?;
            let request = request.adapted_to(&structure.columns);
            let page = db.browse(&request).await?;
            Event::Reloaded(Box::new(structure), page, Box::new(request))
        }
        Command::Browse(request) => Event::Page(db.browse(&request).await?),
        Command::Count(request) => {
            let count = db.count(&request).await?;
            Event::Counted(request.relation, request.filter, count)
        }
        Command::Update(target, original, column, value) => {
            db.update_cell(&target, &original, column, value).await?;
            Event::Mutated
        }
        Command::UpdateRow(target, original, changes) => {
            db.update_row(&target, &original, &changes).await?;
            Event::Mutated
        }
        Command::Insert(target, values) => {
            db.insert_row(&target, &values).await?;
            Event::Mutated
        }
        Command::InsertRows(target, rows) => {
            db.insert_rows(&target, &rows).await?;
            Event::Mutated
        }
        Command::Delete(target, originals) => {
            db.delete_rows(&target, &originals).await?;
            Event::Mutated
        }
        Command::ApplyChanges(target, changes) => {
            db.apply_changes(&target, &changes).await?;
            Event::Mutated
        }
        Command::ExportTable(request, path) => {
            let rows = db.export_table_csv(&request, &path, &progress).await?;
            Event::Exported(rows, path)
        }
        Command::ExportQuery(_, query, path) => {
            let rows = db.export_query_csv(&query, &path, &progress).await?;
            Event::Exported(rows, path)
        }
        Command::Routines => Event::Routines(db.routines().await?),
        Command::Activity => Event::Activity(db.activity().await?),
        Command::IndexReport => Event::IndexReport(db.index_report().await?),
        Command::SlowQueries(request) => Event::SlowQueries(db.slow_queries(request).await?),
        Command::EnableQueryStats(request) => {
            db.enable_query_stats().await?;
            Event::SlowQueries(db.slow_queries(request).await?)
        }
        Command::ConfigureQueryStats(request) => {
            let facts = db.preload_facts().await?;
            ensure_configurable(db, &facts)?;
            let configured = facts.configured.unwrap_or_default();
            db.configure_preload(&sql::with_library(
                &configured.libraries,
                STATEMENTS_LIBRARY,
            ))
            .await?;
            Event::SlowQueries(db.slow_queries(request).await?)
        }
        Command::ResetQueryStats(request) => {
            db.reset_query_stats().await?;
            Event::SlowQueries(db.slow_queries(request).await?)
        }
        Command::SignalBackend(pid, terminate) => {
            Event::Signalled(pid, db.signal_backend(pid, terminate).await?)
        }
        Command::Roles => Event::Roles(db.roles().await?),
        Command::LookupKeys(relation, column, search) => {
            let rows = db.lookup_keys(&relation, &column, &search).await?;
            Event::KeyCandidates(search, rows)
        }
        Command::RoutineDefinition(oid) => {
            Event::RoutineDefinition(oid, db.routine_definition(oid).await?)
        }
        Command::Execute {
            statements,
            transactional,
            description,
        } => {
            db.execute(&statements, transactional).await?;
            Event::Executed(description)
        }
        Command::ImportCsv(import) => Event::Imported(
            db.import_csv(
                &import.relation,
                &import.columns,
                &import.path,
                import.header,
                import.delimiter,
            )
            .await?,
        ),
        Command::Connect(..)
        | Command::Test(..)
        | Command::DiscoverDatabases(..)
        | Command::SwitchDatabase(_)
        | Command::SetUpQueryStats(_)
        | Command::Disconnect
        | Command::CloseSession(_) => unreachable!("control commands do not run on a session"),
    })
}

async fn control(
    command: Command,
    shared: &Shared,
    notify: &(impl Fn(Event) + Sync),
) -> Result<Event> {
    match command {
        Command::Connect(..) | Command::SwitchDatabase(_) | Command::Disconnect => {
            // Wait for every session: lock order is data, the session map, each
            // session, and the connection context.
            let mut data = shared.data.lock().await;
            let mut sessions = shared.sessions.lock().await;
            let slots: Vec<Slot> = sessions.values().cloned().collect();
            let mut guards = Vec::with_capacity(slots.len());
            for slot in &slots {
                guards.push(slot.lock().await);
            }
            let mut context = shared.context.lock().await;
            match command {
                Command::Connect(profile, password) => {
                    connect_sessions(
                        profile,
                        password,
                        &mut data,
                        &mut sessions,
                        &mut context,
                        notify,
                    )
                    .await
                }
                Command::SwitchDatabase(name) => {
                    let current = data.as_ref().context("Not connected.")?;
                    for session in guards.iter().filter_map(|guard| guard.as_ref()) {
                        current
                            .ensure_session_idle(session, "switching databases")
                            .await?;
                    }
                    let mut profile = current.profile.clone();
                    profile.database = name;
                    let password = context.password.clone();
                    connect_sessions(
                        profile,
                        password,
                        &mut data,
                        &mut sessions,
                        &mut context,
                        notify,
                    )
                    .await
                }
                _ => {
                    *data = None;
                    for guard in &mut guards {
                        **guard = None;
                    }
                    sessions.clear();
                    *context = SessionContext::default();
                    Ok(Event::Disconnected)
                }
            }
        }
        Command::SetUpQueryStats(request) => {
            let mut data = shared.data.lock().await;
            let sessions = shared.sessions.lock().await;
            let entries: Vec<(Uuid, Slot)> = sessions
                .iter()
                .map(|(id, slot)| (*id, slot.clone()))
                .collect();
            let mut guards = Vec::with_capacity(entries.len());
            for (_, slot) in &entries {
                guards.push(slot.lock().await);
            }
            let mut context = shared.context.lock().await;
            let db = data.as_ref().context("Not connected.")?;
            for session in guards.iter().filter_map(|guard| guard.as_ref()) {
                db.ensure_session_idle(session, "restarting the server")
                    .await?;
            }
            let facts = db.preload_facts().await?;
            ensure_configurable(db, &facts)?;
            let previous = facts.configured.clone().unwrap_or_default();
            let running = facts.running.clone().unwrap_or_default();
            if !sql::loads_library(&previous.libraries, STATEMENTS_LIBRARY) {
                db.configure_preload(&sql::with_library(&previous.libraries, STATEMENTS_LIBRARY))
                    .await?;
            }
            if !sql::loads_library(&running, STATEMENTS_LIBRARY) {
                let directory = facts
                    .data_directory
                    .clone()
                    .context("Restarting the server requires a superuser.")?;
                let local = match LocalServer::detect(&directory, facts.backend_pid).await {
                    Ok(local) => local,
                    Err(reason) => {
                        db.restore_preload(&previous).await?;
                        bail!(reason);
                    }
                };
                ensure!(
                    server::library_installed(&facts.library_directories, STATEMENTS_LIBRARY),
                    "The pg_stat_statements library was not found on this server."
                );
                notify(Event::Progress("Restarting the server...".into()));
                let profile = db.profile.clone();
                // Close this application's sessions before the shutdown.
                *data = None;
                for guard in &mut guards {
                    **guard = None;
                }
                let restarted = local
                    .restart_with_library(STATEMENTS_LIBRARY, &running)
                    .await;
                let port = context.tunnel_port(&profile).await?;
                let password = context.password.clone();
                let reopened = async {
                    let connection = Database::connect_session(
                        profile.clone(),
                        &password,
                        SessionKind::Data,
                        port,
                    )
                    .await?;
                    let shared_sql = Database::connect_session(
                        profile.clone(),
                        &password,
                        SessionKind::Sql,
                        port,
                    )
                    .await?;
                    anyhow::Ok((connection, shared_sql))
                }
                .await;
                if let Ok((connection, shared_sql)) = reopened {
                    if restarted.is_err() {
                        connection.restore_preload(&previous).await?;
                    }
                    *data = Some(connection);
                    let mut shared_sql = Some(shared_sql);
                    for ((id, _), guard) in entries.iter().zip(guards.iter_mut()) {
                        if *id == SHARED_SESSION {
                            **guard = shared_sql.take();
                        }
                    }
                } else if restarted.is_ok() {
                    reopened?;
                }
                restarted?;
                notify(Event::Notice(
                    "The server was restarted and now loads pg_stat_statements. SQL documents reconnect on their next query, with session settings reset.".into(),
                ));
            }
            let db = data.as_ref().context("Not connected.")?;
            db.enable_query_stats().await?;
            Ok(Event::SlowQueries(db.slow_queries(request).await?))
        }
        Command::DiscoverDatabases(profile, password) => {
            let mut temporary = SessionContext::default();
            let port = temporary.tunnel_port(&profile).await?;
            let databases =
                Database::discover_databases_via(profile.clone(), &password, port).await?;
            Ok(Event::DiscoveredDatabases(profile, databases))
        }
        Command::Test(profile, password) => {
            let mut temporary = SessionContext::default();
            let port = temporary.tunnel_port(&profile).await?;
            let connection =
                Database::connect_session(profile, &password, SessionKind::Data, port).await?;
            connection.client.simple_query("SELECT 1").await?;
            Ok(Event::Tested)
        }
        _ => unreachable!("session commands do not run on the control lane"),
    }
}

/// Opens the data session and the shared SQL session before replacing the current
/// ones, so a failed connection or database switch leaves the workspace connected.
/// Document sessions are closed and reopen on their next query.
async fn connect_sessions(
    profile: ConnectionProfile,
    password: String,
    data: &mut Option<Database>,
    sessions: &mut HashMap<Uuid, Slot>,
    context: &mut SessionContext,
    notify: &(impl Fn(Event) + Sync),
) -> Result<Event> {
    let mut next = SessionContext {
        profile: Some(profile.clone()),
        password: password.clone(),
        tunnel: None,
    };
    let reuse = profile.ssh.as_ref().is_some_and(|ssh| {
        context.tunnel.as_mut().is_some_and(|tunnel| {
            tunnel.serves(ssh, &profile.host, profile.port) && tunnel.is_alive()
        })
    });
    let port = if reuse {
        next.tunnel = context.tunnel.take();
        next.tunnel.as_ref().map(|tunnel| tunnel.local_port)
    } else {
        next.tunnel_port(&profile).await?
    };
    let opened = async {
        let connection =
            Database::connect_session(profile.clone(), &password, SessionKind::Data, port).await?;
        let query_connection =
            Database::connect_session(profile.clone(), &password, SessionKind::Sql, port).await?;
        let relations = connection.relations().await?;
        anyhow::Ok((connection, query_connection, relations))
    }
    .await;
    match opened {
        Ok((connection, query_connection, relations)) => {
            notify(Event::Routines(
                connection.routines().await.unwrap_or_default(),
            ));
            notify(Event::CompletionColumns(
                connection.completion_columns().await.unwrap_or_default(),
            ));
            *data = Some(connection);
            sessions.clear();
            sessions.insert(SHARED_SESSION, Arc::new(Mutex::new(Some(query_connection))));
            *context = next;
            Ok(Event::Connected(profile, relations))
        }
        Err(error) => {
            if reuse {
                context.tunnel = next.tunnel.take();
            }
            Err(error)
        }
    }
}
