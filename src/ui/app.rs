use super::{display::BinaryDisplay, edits::PendingEdits, grid, theme};
use crate::{
    models::*,
    services::{
        database::line_and_column,
        storage::{self, Store},
        worker::{COUNT_SESSION, Command, CsvImport, Event, Lane, Worker},
    },
    sql::{self, lexer},
};
use anyhow::{Context, Result};
use eframe::egui::{self, RichText};
use std::{
    collections::{BTreeSet, HashMap},
    ops::Range,
    time::{Duration, Instant},
};
use uuid::Uuid;

mod diagnostics;
mod dialogs;
mod editor;
mod objects;
mod server_views;
mod servers;
#[cfg(feature = "smoke")]
mod smoke;
mod structure;
mod table;
mod tabs;
use tabs::TableTab;
mod workspace;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Content,
    Structure,
    Definition,
    Query,
    /// A function or procedure definition.
    Routine,
    /// The structure editor creating a new table.
    NewTable,
    /// Server sessions from pg_stat_activity.
    Activity,
    Roles,
    /// The index advisor and slow queries.
    Diagnostics,
}

/// The structure editor's state: the structure as loaded and as edited.
struct TableEditor {
    original: TableDraft,
    edited: TableDraft,
    creating: bool,
}

/// Which worker lanes are running an operation. The table browser and each SQL
/// document work independently; connecting and switching wait for all of them.
#[derive(Default)]
struct Busy {
    data: bool,
    /// SQL sessions with a running operation, by document.
    sql: BTreeSet<Uuid>,
    control: bool,
}

impl Busy {
    fn any(&self) -> bool {
        self.data || !self.sql.is_empty() || self.control
    }

    /// True when a command for `lane` cannot start now.
    fn blocks(&self, lane: Lane) -> bool {
        match lane {
            Lane::Data => self.data || self.control,
            Lane::Sql(id) => self.sql.contains(&id) || self.control,
            Lane::Control => self.any(),
        }
    }

    fn set(&mut self, lane: Lane, value: bool) {
        match lane {
            Lane::Data => self.data = value,
            Lane::Sql(id) => {
                if value {
                    self.sql.insert(id);
                } else {
                    self.sql.remove(&id);
                }
            }
            Lane::Control => self.control = value,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunScope {
    /// The selection, or the statement at the cursor when nothing is selected.
    Statement,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    Normal,
    /// EXPLAIN; `rollback` means the script began a transaction that must be
    /// rolled back if the statement fails.
    Explain {
        rollback: bool,
    },
    /// A housekeeping statement whose result is not shown.
    Silent,
}

/// A running query: the character offset of the user's text within its document
/// and within the executed script, used to place the cursor at an error.
struct RunningQuery {
    base: usize,
    prefix: usize,
    kind: QueryKind,
}

enum Dialog {
    /// The full value editor. Editable cells stage their change on Apply.
    Cell {
        row: usize,
        column: usize,
        name: String,
        value: String,
        null: bool,
        editable: bool,
        pretty: bool,
    },
    ImportUrl(String),
    RemoveDocument(Uuid),
    CloseTable(Uuid),
    ImportCsv(CsvDraft),
    /// Closing the window with unsaved grid changes.
    QuitWithChanges,
    RelationAction {
        relation: Relation,
        action: RelationAction,
    },
    DropRoutine {
        routine: Routine,
        cascade: bool,
    },
    CreateSchema(String),
    /// Choose a referenced row for a single-column foreign key cell.
    KeyPicker {
        row: usize,
        column: usize,
        key: ForeignKey,
        search: String,
        /// The search whose results were requested last.
        requested: Option<String>,
        results: Vec<(String, String)>,
    },
    MoveToFolder {
        document: Uuid,
        folder: String,
    },
    CreateDatabase(String),
    DropDatabase {
        name: String,
        confirmation: String,
    },
    /// Generated DDL shown for review before it runs in one transaction.
    ReviewDdl {
        statements: Vec<String>,
        description: String,
        reopen: Option<(String, String)>,
    },
    Terminate {
        pid: i32,
        description: String,
    },
    ResetQueryStats,
    /// Confirms configuring pg_stat_statements and restarting the local server.
    SetUpQueryStats {
        data_directory: String,
        log_file: String,
        other_sessions: i64,
        configure: bool,
    },
}

/// A CSV file chosen for import and how its fields map to table columns.
struct CsvDraft {
    path: std::path::PathBuf,
    fields: Vec<String>,
    header: bool,
    delimiter: char,
    /// The table column for each field of the file, in file order.
    mapping: Vec<String>,
}

pub struct PosternApp {
    #[cfg(feature = "smoke")]
    smoke: Option<smoke::Smoke>,
    worker: Worker,
    store: Option<Store>,
    profiles_writable: bool,
    documents_writable: bool,
    history_writable: bool,
    profiles: Vec<ConnectionProfile>,
    draft: ConnectionProfile,
    password: String,
    remember_password: bool,
    server_search: String,
    servers_visible: bool,
    connected: Option<ConnectionProfile>,
    databases: Vec<String>,
    discovered_databases: Option<Vec<String>>,
    relations: Vec<Relation>,
    routines: Vec<Routine>,
    selected_routine: Option<Routine>,
    routine_definition: Option<String>,
    /// Open the selected routine's definition in the SQL editor once it loads.
    open_routine_in_editor: bool,
    activity: Vec<SessionActivity>,
    activity_auto: bool,
    activity_refreshed: Instant,
    roles: Vec<RoleInfo>,
    diagnostics_tab: diagnostics::DiagnosticsTab,
    index_report: Option<IndexReport>,
    slow_queries: Option<SlowQueryReport>,
    slow_request: SlowQueryRequest,
    /// EXPLAIN plans by document.
    plans: HashMap<Uuid, crate::ui::explain::Plan>,
    /// A relation to open once the object list has been refreshed after DDL.
    reopen_after_catalog: Option<(String, String)>,
    ddl_tab: Option<Uuid>,
    refresh_databases_after_ddl: bool,
    relation_search: String,
    table_tabs: Vec<TableTab>,
    table_index: usize,
    /// Stable owners of the operations on the shared data and count lanes.
    data_tab: Option<Uuid>,
    count_tab: Option<Uuid>,
    ui_state: UiState,
    ui_state_writable: bool,
    allow_close: bool,
    mode: Mode,
    documents: Vec<QueryDocument>,
    document_index: usize,
    query_outputs: HashMap<Uuid, QueryOutput>,
    /// Character offset of the executed SQL within each document's text.
    query_base: HashMap<Uuid, usize>,
    result_index: usize,
    results_grid: grid::GridState,
    results_key: Option<(Uuid, usize)>,
    /// Character range of the editor selection and the cursor position.
    editor_selection: Range<usize>,
    editor_cursor: usize,
    /// Running queries by document.
    running: HashMap<Uuid, RunningQuery>,
    /// A cursor position to apply to a document's editor, such as an error location.
    pending_cursor: Option<(Uuid, usize)>,
    /// A character range to select in a document's editor, such as a find match.
    pending_select: Option<(Uuid, Range<usize>)>,
    completion_catalog: crate::sql::complete::CompletionCatalog,
    completion: Option<editor::CompletionPopup>,
    /// Screen position below the editor cursor, where completions appear.
    editor_caret: Option<egui::Pos2>,
    find: Option<editor::FindBar>,
    /// A find step requested from the menu: next (true) or previous.
    pending_find_step: Option<bool>,
    #[cfg(target_os = "macos")]
    menu: Option<super::menu::NativeMenu>,
    /// The application icon, shown on the server list.
    logo: Option<egui::TextureHandle>,
    file_states: HashMap<Uuid, editor::FileState>,
    /// File-backed documents whose file changed while the editor had other edits.
    file_conflicts: BTreeSet<Uuid>,
    last_file_check: Instant,
    /// Transaction state of each document's SQL session.
    transaction: HashMap<Uuid, TransactionState>,
    dirty_since: Option<Instant>,
    history: Vec<String>,
    show_history: bool,
    busy: Busy,
    status: String,
    /// A non-blocking message shown in the status bar.
    warning: Option<String>,
    error: Option<String>,
    dialog: Option<Dialog>,
}

impl PosternApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        theme::apply(&cc.egui_ctx);
        #[allow(unused_mut)]
        let mut app = Self::from_context(&cc.egui_ctx, Store::open())?;
        #[cfg(target_os = "macos")]
        {
            app.menu = super::menu::NativeMenu::install(&cc.egui_ctx);
        }
        #[cfg(feature = "smoke")]
        app.init_smoke();
        Ok(app)
    }

    fn from_context(ctx: &egui::Context, store_result: Result<Store>) -> Result<Self> {
        let context = ctx.clone();
        let worker = Worker::start(move || context.request_repaint())?;
        let mut errors = Vec::new();
        let store = match store_result {
            Ok(store) => Some(store),
            Err(error) => {
                errors.push(error.to_string());
                None
            }
        };
        let mut profiles = Vec::new();
        let mut documents = Vec::new();
        let mut history = Vec::new();
        let mut ui_state = UiState::default();
        let (mut profiles_writable, mut documents_writable, mut history_writable) =
            (false, false, false);
        let mut ui_state_writable = false;
        if let Some(store) = &store {
            match store.profiles() {
                Ok(value) => {
                    profiles = value;
                    profiles_writable = true;
                }
                Err(e) => errors.push(e.to_string()),
            }
            match store.documents() {
                Ok(value) => {
                    documents = value;
                    documents_writable = true;
                }
                Err(e) => errors.push(e.to_string()),
            }
            match store.history() {
                Ok(value) => {
                    history = value;
                    history_writable = true;
                }
                Err(e) => errors.push(e.to_string()),
            }
            match store.ui_state() {
                Ok(value) => {
                    ui_state = value;
                    ui_state_writable = true;
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        if documents.is_empty() {
            documents.push(QueryDocument::default());
        }
        let draft = profiles.first().cloned().unwrap_or_default();
        let app = Self {
            #[cfg(feature = "smoke")]
            smoke: None,
            worker,
            store,
            profiles_writable,
            documents_writable,
            history_writable,
            profiles,
            draft,
            password: String::new(),
            remember_password: false,
            server_search: String::new(),
            servers_visible: true,
            connected: None,
            databases: vec![],
            discovered_databases: None,
            relations: vec![],
            routines: vec![],
            selected_routine: None,
            routine_definition: None,
            open_routine_in_editor: false,
            activity: Vec::new(),
            activity_auto: false,
            activity_refreshed: Instant::now(),
            roles: Vec::new(),
            diagnostics_tab: diagnostics::DiagnosticsTab::default(),
            index_report: None,
            slow_queries: None,
            slow_request: SlowQueryRequest::default(),
            plans: HashMap::new(),
            reopen_after_catalog: None,
            ddl_tab: None,
            refresh_databases_after_ddl: false,
            relation_search: String::new(),
            table_tabs: vec![TableTab::default()],
            table_index: 0,
            data_tab: None,
            count_tab: None,
            ui_state,
            ui_state_writable,
            allow_close: false,
            mode: Mode::Query,
            documents,
            document_index: 0,
            query_outputs: HashMap::new(),
            query_base: HashMap::new(),
            result_index: 0,
            results_grid: grid::GridState::default(),
            results_key: None,
            editor_selection: 0..0,
            editor_cursor: 0,
            running: HashMap::new(),
            pending_cursor: None,
            pending_select: None,
            completion_catalog: Default::default(),
            completion: None,
            editor_caret: None,
            find: None,
            pending_find_step: None,
            #[cfg(target_os = "macos")]
            menu: None,
            logo: theme::logo(ctx),
            file_states: HashMap::new(),
            file_conflicts: BTreeSet::new(),
            last_file_check: Instant::now() - Duration::from_secs(10),
            transaction: HashMap::new(),
            dirty_since: None,
            history,
            show_history: false,
            busy: Busy::default(),
            status: "Ready to connect".into(),
            warning: None,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("\n"))
            },
            dialog: None,
        };
        Ok(app)
    }

    /// Sends a command when its lane is free. Returns false when it was not sent.
    fn send(&mut self, command: Command, status: &str) -> bool {
        let sent = self.dispatch(command);
        if sent {
            self.status = status.into();
            self.error = None;
            self.warning = None;
        }
        sent
    }

    /// Sends a command when its lane is free, without touching the status line.
    fn dispatch(&mut self, command: Command) -> bool {
        if matches!(
            &command,
            Command::Connect(..) | Command::SwitchDatabase(_) | Command::Disconnect
        ) && self.blocked_by_any_edits()
        {
            return false;
        }
        let lane = command.lane();
        if self.busy.blocks(lane) {
            return false;
        }
        let table_command = matches!(
            &command,
            Command::Load(..)
                | Command::Reload(_)
                | Command::Browse(_)
                | Command::ApplyChanges(..)
                | Command::ImportCsv(_)
        );
        let count_command = matches!(&command, Command::Count(_));
        match self.worker.send(command) {
            Ok(()) => {
                self.busy.set(lane, true);
                if lane == Lane::Data {
                    self.data_tab = table_command.then_some(self.table_tabs[self.table_index].id);
                }
                if count_command {
                    self.count_tab = Some(self.table_tabs[self.table_index].id);
                }
                true
            }
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }

    /// Counts the rows matching the current filter on the hidden count session,
    /// so browsing is never delayed. Counting starts on its own for tables
    /// estimated below `AUTO_COUNT_LIMIT` rows; `manual` counts any table.
    fn request_count(&mut self, manual: bool) {
        const AUTO_COUNT_LIMIT: i64 = 2_000_000;
        let Some(relation) = &self.table_tabs[self.table_index].selected_relation else {
            return;
        };
        if self.table_tabs[self.table_index]
            .structure
            .columns
            .is_empty()
            || (!manual && self.table_tabs[self.table_index].row_count.is_some())
        {
            return;
        }
        let estimate = self
            .relations
            .iter()
            .find(|r| r.schema == relation.schema && r.name == relation.name)
            .map_or(relation.estimated_rows, |r| r.estimated_rows);
        if !manual && estimate > AUTO_COUNT_LIMIT {
            return;
        }
        if self.busy.blocks(Lane::Sql(COUNT_SESSION)) {
            self.table_tabs[self.table_index].count_wanted = true;
            return;
        }
        self.table_tabs[self.table_index].count_wanted = false;
        if let Some(request) = self.browse_request(0) {
            self.dispatch(Command::Count(request));
        }
    }

    /// After a page arrives: the last page gives the exact total for free;
    /// otherwise the total is counted.
    fn update_row_count(&mut self) {
        if !self.table_tabs[self.table_index].table_page.has_more
            && !self.table_tabs[self.table_index]
                .table_page
                .result
                .columns
                .is_empty()
        {
            let total = self.table_tabs[self.table_index].page
                * self.table_tabs[self.table_index].page_size
                + self.table_tabs[self.table_index]
                    .table_page
                    .result
                    .rows
                    .len();
            self.table_tabs[self.table_index].row_count = Some(total as i64);
            if self.count_tab == Some(self.table_tabs[self.table_index].id)
                && self.busy.blocks(Lane::Sql(COUNT_SESSION))
            {
                self.worker.cancel_lane(Lane::Sql(COUNT_SESSION));
            }
            self.table_tabs[self.table_index].count_wanted = false;
        } else {
            self.request_count(false);
        }
    }

    fn receive(&mut self) {
        while let Some((lane, event)) = self.worker.poll_lane() {
            self.handle_event(lane, event);
        }
        self.load_waiting_table();
        self.count_waiting_table();
    }

    fn handle_event(&mut self, lane: Lane, event: Event) {
        if event.finishes() {
            self.busy.set(lane, false);
        }
        let table_event = matches!(
            &event,
            Event::Loaded(..)
                | Event::Reloaded(..)
                | Event::Page(..)
                | Event::Mutated
                | Event::Imported(_)
                | Event::Counted(..)
        ) || matches!(&event, Event::Error(_))
            && (lane == Lane::Data || lane == Lane::Sql(COUNT_SESSION));
        let visible = self.table_tabs[self.table_index].id;
        if table_event {
            // Table handlers and their follow-up commands use table_index.
            // Restore the visible tab after handling the event synchronously.
            let owner = if lane == Lane::Sql(COUNT_SESSION) {
                self.count_tab
            } else {
                self.data_tab
            };
            if let Some(owner) = owner {
                let Some(index) = self.table_tabs.iter().position(|tab| tab.id == owner) else {
                    return;
                };
                self.table_index = index;
            } else if !matches!(&event, Event::Error(_)) {
                return;
            }
        }
        match event {
            Event::Connected(profile, relations) => {
                self.clear_table_tabs();
                self.completion_catalog.relations = relations
                    .iter()
                    .map(|r| (r.schema.clone(), r.name.clone()))
                    .collect();
                self.connected = Some(profile);
                self.relations = relations;
                self.servers_visible = false;
                self.query_outputs.clear();
                self.editor_selection = 0..0;
                self.dialog = None;
                self.databases.clear();
                self.transaction.clear();
                self.running.clear();
                self.index_report = None;
                self.slow_queries = None;
                self.mode = Mode::Query;
                self.status = "Connected".into();
                self.send(Command::Databases, "Loading databases...");
            }
            Event::DiscoveredDatabases(profile, databases) => {
                if profile == self.draft {
                    self.status = format!(
                        "Found {} databases. Choose one in Connection details.",
                        databases.len()
                    );
                    self.discovered_databases = Some(databases);
                }
            }
            Event::Databases(databases) => {
                self.status = format!("{} databases available", databases.len());
                self.databases = databases;
            }
            Event::Tested => self.status = "Connection test succeeded".into(),
            Event::Disconnected => {
                self.clear_table_tabs();
                self.connected = None;
                self.databases.clear();
                self.relations.clear();
                self.query_outputs.clear();
                self.servers_visible = true;
                self.transaction.clear();
                self.running.clear();
                self.status = "Disconnected".into();
            }
            Event::Catalog(relations) => {
                self.completion_catalog.relations = relations
                    .iter()
                    .map(|r| (r.schema.clone(), r.name.clone()))
                    .collect();
                self.relations = relations;
                self.status = "Object list refreshed".into();
                self.reopen_table_after_ddl();
                self.reconcile_table_tabs();
            }
            Event::Routines(routines) => {
                if let Some(selected) = &self.selected_routine
                    && !routines.iter().any(|r| r.oid == selected.oid)
                {
                    self.selected_routine = None;
                    if self.mode == Mode::Routine {
                        self.mode = Mode::Query;
                    }
                }
                self.completion_catalog.functions = routines
                    .iter()
                    .filter(|r| !r.procedure)
                    .map(|r| (r.schema.clone(), r.name.clone()))
                    .collect();
                self.routines = routines;
            }
            Event::CompletionColumns(columns) => self.completion_catalog.columns = columns,
            Event::RoutineDefinition(oid, definition) => {
                if self.selected_routine.as_ref().is_some_and(|r| r.oid == oid) {
                    self.routine_definition = Some(definition);
                    self.status = "Definition loaded".into();
                }
            }
            Event::KeyCandidates(search, rows) => {
                if let Some(Dialog::KeyPicker {
                    search: current,
                    results,
                    ..
                }) = &mut self.dialog
                    && *current == search
                {
                    *results = rows;
                }
                self.status = "Ready".into();
            }
            Event::Executed(description) => {
                self.status = description;
                if std::mem::take(&mut self.refresh_databases_after_ddl) {
                    self.send(Command::Databases, "Refreshing databases...");
                } else {
                    if let Some(tab) = self
                        .table_tabs
                        .iter_mut()
                        .find(|tab| Some(tab.id) == self.ddl_tab)
                    {
                        tab.table_editor = None;
                    }
                    self.send(Command::Catalog, "Refreshing objects...");
                }
            }
            Event::Loaded(structure, page) => {
                self.table_tabs[self.table_index].loading_rows = false;
                self.table_tabs[self.table_index].structure = *structure;
                self.table_tabs[self.table_index].table_page = page;
                self.table_tabs[self.table_index].page = 0;
                self.status = format!(
                    "Loaded in {} ms",
                    self.table_tabs[self.table_index].table_page.elapsed_ms
                );
                self.update_row_count();
            }
            Event::Reloaded(structure, page, request) => {
                self.table_tabs[self.table_index].loading_rows = false;
                let request = *request;
                self.table_tabs[self.table_index].structure = *structure;
                self.table_tabs[self.table_index].table_page = page;
                self.table_tabs[self.table_index].sort = request.sort;
                self.table_tabs[self.table_index].show_filter |= !request.filter.is_empty();
                self.table_tabs[self.table_index].filter = request.filter.clone();
                self.table_tabs[self.table_index].applied_filter = request.filter;
                self.table_tabs[self.table_index].page = request.page;
                self.table_tabs[self.table_index].content.clear();
                self.status = format!(
                    "Refreshed in {} ms",
                    self.table_tabs[self.table_index].table_page.elapsed_ms
                );
                self.update_row_count();
            }
            Event::Page(page) => {
                self.table_tabs[self.table_index].loading_rows = false;
                self.table_tabs[self.table_index].table_page = page;
                self.table_tabs[self.table_index].page =
                    self.table_tabs[self.table_index].pending_page;
                self.status = format!(
                    "Loaded in {} ms",
                    self.table_tabs[self.table_index].table_page.elapsed_ms
                );
                self.update_row_count();
            }
            Event::Counted(relation, filter, count) => {
                let current = self.table_tabs[self.table_index]
                    .selected_relation
                    .as_ref()
                    .is_some_and(|selected| {
                        selected.schema == relation.schema && selected.name == relation.name
                    });
                if current
                    && filter == self.table_tabs[self.table_index].applied_filter
                    && !self.table_tabs[self.table_index].count_wanted
                {
                    self.table_tabs[self.table_index].row_count = Some(count);
                }
                if self.table_tabs[self.table_index].count_wanted {
                    self.request_count(true);
                }
            }
            Event::Query(output) => {
                self.status = if output.error.is_some() {
                    "Query failed".into()
                } else {
                    format!("Finished in {} ms", output.elapsed_ms)
                };
                if let Lane::Sql(document) = lane
                    && let Some(running) = self.running.remove(&document)
                {
                    let mut output = output;
                    self.transaction.insert(document, output.transaction);
                    if running.kind == QueryKind::Silent {
                        return;
                    }
                    output.error_offset = output
                        .error_offset
                        .map(|offset| offset.saturating_sub(running.prefix));
                    if let Some(offset) = output.error_offset {
                        self.pending_cursor = Some((document, running.base + offset));
                    }
                    if let QueryKind::Explain { rollback } = running.kind {
                        if let Some(plan) = output
                            .results
                            .iter()
                            .find(|r| r.columns == ["QUERY PLAN"])
                            .and_then(|r| r.rows.first()?.first()?.clone())
                            .and_then(|json| crate::ui::explain::parse(&json))
                        {
                            self.plans.insert(document, plan);
                        }
                        if rollback
                            && output.error.is_some()
                            && self.send(
                                Command::QueryIn(document, "ROLLBACK".into()),
                                "Rolling back...",
                            )
                        {
                            self.running.insert(
                                document,
                                RunningQuery {
                                    base: 0,
                                    prefix: 0,
                                    kind: QueryKind::Silent,
                                },
                            );
                        }
                    } else {
                        self.plans.remove(&document);
                    }
                    self.query_base.insert(document, running.base);
                    self.query_outputs.insert(document, output);
                    if self
                        .documents
                        .get(self.document_index)
                        .is_some_and(|d| d.id == document)
                    {
                        self.result_index = 0;
                    }
                }
            }
            Event::Mutated => {
                self.table_tabs[self.table_index].edits.clear();
                self.table_tabs[self.table_index].row_count = None;
                self.status = "Changes saved".into();
                self.browse(self.table_tabs[self.table_index].page);
            }
            Event::Exported(rows, path) => {
                self.status = format!("Exported {rows} rows to {}", path.display());
            }
            Event::Imported(rows) => {
                self.status = format!("Imported {rows} rows");
                self.table_tabs[self.table_index].row_count = None;
                self.browse(self.table_tabs[self.table_index].page);
            }
            Event::Progress(message) => self.status = message,
            Event::SessionClosed => {}
            Event::Activity(sessions) => {
                self.status = format!("{} sessions", sessions.len());
                self.activity = sessions;
            }
            Event::Signalled(pid, delivered) => {
                self.status = if delivered {
                    format!("Signal sent to process {pid}")
                } else {
                    format!("Process {pid} was not found or could not be signalled")
                };
                if self.mode == Mode::Activity {
                    self.send(Command::Activity, "Refreshing sessions...");
                }
            }
            Event::Roles(roles) => {
                self.status = format!("{} roles", roles.len());
                self.roles = roles;
            }
            Event::IndexReport(report) => {
                self.status = match report.items.len() {
                    0 => "No index problems found".into(),
                    1 => "1 index advisor finding".into(),
                    n => format!("{n} index advisor findings"),
                };
                self.index_report = Some(report);
            }
            Event::SlowQueries(report) => {
                self.status = match &report.status {
                    StatementsStatus::Ready => format!("{} statements", report.queries.len()),
                    _ => "Query statistics are not available".into(),
                };
                self.slow_queries = Some(report);
            }
            Event::Notice(message) => self.warning = Some(message),
            Event::Error(_) if lane == Lane::Sql(COUNT_SESSION) => {
                // Counting is best effort: a cancelled, timed-out, or failed
                // count leaves the total unknown without interrupting the user.
                if self.table_tabs[self.table_index].count_wanted {
                    self.request_count(true);
                }
            }
            Event::Error(error) => {
                if lane == Lane::Data
                    && self.data_tab.is_some()
                    && std::mem::take(&mut self.table_tabs[self.table_index].loading_rows)
                {
                    // The previous rows no longer match the requested page.
                    self.table_tabs[self.table_index].table_page = TablePage::default();
                }
                if lane == Lane::Data || lane == Lane::Control {
                    self.reopen_after_catalog = None;
                    self.ddl_tab = None;
                    self.refresh_databases_after_ddl = false;
                }
                if let Lane::Sql(document) = lane {
                    self.running.remove(&document);
                    self.transaction.remove(&document);
                }
                self.error = Some(error);
                self.status = "Operation failed".into();
            }
        }
        if table_event && let Some(index) = self.table_tabs.iter().position(|tab| tab.id == visible)
        {
            self.table_index = index;
        }
    }

    /// True, with a warning, when staged grid changes would be lost.
    fn blocked_by_edits(&mut self) -> bool {
        self.stage_inline_edit();
        if self.table_tabs[self.table_index].edits.is_empty() {
            return false;
        }
        self.warning = Some(format!(
            "Save or discard {} pending {} first (Command-S saves).",
            self.table_tabs[self.table_index].edits.len(),
            if self.table_tabs[self.table_index].edits.len() == 1 {
                "change"
            } else {
                "changes"
            }
        ));
        true
    }

    fn select_relation(&mut self, relation: Relation) {
        if self.busy.control || self.dialog.is_some() {
            return;
        }
        if let Some(index) = self.table_tabs.iter().position(|tab| {
            tab.selected_relation
                .as_ref()
                .is_some_and(|open| open.schema == relation.schema && open.name == relation.name)
        }) {
            self.select_table_tab(index);
            return;
        }
        let tab = TableTab {
            selected_relation: Some(relation),
            page_size: self.table_tabs[self.table_index].page_size,
            needs_load: true,
            loading_rows: true,
            ..Default::default()
        };
        self.add_table_tab(tab);
        self.load_waiting_table();
    }

    fn browse_request(&self, page: usize) -> Option<BrowseRequest> {
        Some(BrowseRequest {
            relation: self.table_tabs[self.table_index]
                .selected_relation
                .clone()?,
            columns: self.table_tabs[self.table_index].structure.columns.clone(),
            filter: self.table_tabs[self.table_index].applied_filter.clone(),
            sort: self.table_tabs[self.table_index].sort.clone(),
            nulls: self.table_tabs[self.table_index].nulls,
            page,
            page_size: self.table_tabs[self.table_index].page_size,
        })
    }

    fn browse(&mut self, page: usize) {
        if self.busy.blocks(Lane::Data) || self.blocked_by_edits() {
            return;
        }
        if let Some(request) = self.browse_request(page) {
            self.table_tabs[self.table_index].pending_page = page;
            self.table_tabs[self.table_index].content.clear();
            self.table_tabs[self.table_index].loading_rows =
                self.send(Command::Browse(request), "Loading rows...");
        }
    }

    /// Reloads the selected table's metadata and rows, keeping its filter, sort,
    /// and page. Without a selected table, refreshes the object list.
    fn refresh(&mut self) {
        if self.busy.blocks(Lane::Data) || self.connected.is_none() {
            return;
        }
        if self.mode == Mode::Diagnostics && !self.servers_visible {
            self.refresh_diagnostics();
            return;
        }
        if self.blocked_by_edits() {
            return;
        }
        match (
            self.table_tabs[self.table_index].selected_relation.clone(),
            self.table_tabs[self.table_index]
                .structure
                .columns
                .is_empty(),
        ) {
            (Some(relation), true) => {
                self.table_tabs[self.table_index].loading_rows = self.send(
                    Command::Load(relation, self.table_tabs[self.table_index].page_size),
                    "Loading table...",
                );
            }
            (Some(_), false) => {
                if let Some(request) = self.browse_request(self.table_tabs[self.table_index].page) {
                    self.table_tabs[self.table_index].content.clear();
                    self.table_tabs[self.table_index].row_count = None;
                    self.send(Command::Reload(request), "Refreshing table...");
                }
            }
            (None, _) => {
                self.send(Command::Catalog, "Refreshing objects...");
            }
        }
    }

    fn apply_filter(&mut self, filter: FilterSet) {
        if self.blocked_by_edits() {
            return;
        }
        self.table_tabs[self.table_index].applied_filter = filter;
        self.table_tabs[self.table_index].row_count = None;
        self.browse(0);
    }

    /// Opens a related table filtered to the given rows, remembering the current
    /// table for Back.
    fn open_related(&mut self, relation: Relation, filter: FilterSet) {
        if self.busy.blocks(Lane::Data) || self.blocked_by_edits() {
            return;
        }
        let previous = self.browse_request(self.table_tabs[self.table_index].page);
        let request = BrowseRequest {
            relation: relation.clone(),
            filter,
            nulls: self.table_tabs[self.table_index].nulls,
            page_size: self.table_tabs[self.table_index].page_size,
            ..Default::default()
        };
        if self.send(Command::Reload(request), "Opening related rows...") {
            if let Some(previous) = previous {
                self.table_tabs[self.table_index].back_stack.push(previous);
            }
            self.table_tabs[self.table_index].selected_relation = Some(relation);
            self.mode = Mode::Content;
            self.table_tabs[self.table_index].table_page = TablePage::default();
            self.table_tabs[self.table_index].structure = TableStructure::default();
            self.table_tabs[self.table_index].row_count = None;
            self.table_tabs[self.table_index].content.clear();
        }
    }

    fn go_back(&mut self) {
        if self.busy.blocks(Lane::Data) || self.blocked_by_edits() {
            return;
        }
        if let Some(request) = self.table_tabs[self.table_index].back_stack.pop() {
            self.table_tabs[self.table_index].selected_relation = Some(request.relation.clone());
            self.mode = Mode::Content;
            self.table_tabs[self.table_index].row_count = None;
            self.table_tabs[self.table_index].content.clear();
            self.send(Command::Reload(request), "Going back...");
        }
    }

    /// Runs DDL on the table browser's session. `reopen` is the relation to open
    /// once the object list is refreshed.
    fn run_ddl(
        &mut self,
        statements: Vec<String>,
        transactional: bool,
        description: &str,
        reopen: Option<(String, String)>,
    ) {
        if statements.is_empty() || self.blocked_by_any_row_edits() {
            return;
        }
        if self.send(
            Command::Execute {
                statements,
                transactional,
                description: description.into(),
            },
            "Running...",
        ) {
            let tab = &self.table_tabs[self.table_index];
            self.ddl_tab = tab
                .table_editor
                .as_ref()
                .filter(|editor| {
                    reopen.as_ref().is_some_and(|(schema, name)| {
                        *schema == editor.edited.schema && name == editor.edited.name.trim()
                    })
                })
                .map(|_| tab.id);
            self.reopen_after_catalog = reopen;
        }
    }

    fn open_routine(&mut self, routine: Routine) {
        if self.busy.blocks(Lane::Data) || self.blocked_by_edits() {
            return;
        }
        let oid = routine.oid;
        self.selected_routine = Some(routine);
        self.routine_definition = None;
        self.mode = Mode::Routine;
        self.send(Command::RoutineDefinition(oid), "Loading definition...");
    }

    /// Opens SQL in a new editor document.
    fn open_in_editor(&mut self, title: String, sql: String) {
        self.documents.push(QueryDocument {
            title,
            sql,
            ..Default::default()
        });
        self.select_document(self.documents.len() - 1);
        self.save_drafts();
    }

    fn save_changes(&mut self) {
        if self.busy.blocks(Lane::Data) {
            return;
        }
        self.stage_inline_edit();
        if self.table_tabs[self.table_index].edits.is_empty() {
            return;
        }
        let Some(relation) = self.table_tabs[self.table_index].selected_relation.clone() else {
            return;
        };
        let changes = self.table_tabs[self.table_index]
            .edits
            .to_changes(&self.table_tabs[self.table_index].table_page.result.rows);
        self.send(
            Command::ApplyChanges(
                MutationTarget {
                    relation,
                    columns: self.table_tabs[self.table_index].structure.columns.clone(),
                },
                changes,
            ),
            "Saving changes...",
        );
    }

    fn discard_changes(&mut self) {
        if self.busy.blocks(Lane::Data) {
            return;
        }
        self.table_tabs[self.table_index].edits.clear();
        self.table_tabs[self.table_index].content.editor = None;
        self.status = "Changes discarded".into();
    }

    /// Key for per-table settings: the connection, database, and table.
    fn table_key(&self) -> Option<String> {
        let profile = self.connected.as_ref()?;
        let relation = self.table_tabs[self.table_index]
            .selected_relation
            .as_ref()?;
        Some(format!(
            "{}/{}/{}",
            profile.id,
            profile.database,
            relation.qualified_name()
        ))
    }

    fn hidden_columns(&self) -> BTreeSet<String> {
        self.table_key()
            .and_then(|key| self.ui_state.hidden_columns.get(&key).cloned())
            .unwrap_or_default()
    }

    fn set_hidden_columns(&mut self, hidden: BTreeSet<String>) {
        let Some(key) = self.table_key() else {
            return;
        };
        if hidden.is_empty() {
            self.ui_state.hidden_columns.remove(&key);
        } else {
            self.ui_state.hidden_columns.insert(key, hidden);
        }
        self.save_ui_state();
    }

    fn save_ui_state(&mut self) {
        if self.ui_state_writable
            && let Some(store) = &self.store
            && let Err(error) = store.save_ui_state(&self.ui_state)
        {
            self.warning = Some(format!("Display settings could not be saved: {error}"));
        }
    }

    fn can_edit(&self) -> bool {
        !self.busy.blocks(Lane::Data)
            && self.table_tabs[self.table_index]
                .selected_relation
                .as_ref()
                .is_some_and(Relation::is_table)
            && self.table_tabs[self.table_index]
                .structure
                .columns
                .iter()
                .any(|c| c.primary_key)
    }

    /// Marks the selected loaded rows for deletion and removes selected new rows.
    /// Loaded rows need a primary key; nothing is deleted until changes are saved.
    fn stage_delete(&mut self) {
        if self.mode != Mode::Content || self.servers_visible || self.dialog.is_some() {
            return;
        }
        let loaded = self.table_tabs[self.table_index]
            .table_page
            .result
            .rows
            .len();
        let selected: Vec<usize> = self.table_tabs[self.table_index]
            .content
            .selection
            .indices()
            .collect();
        if selected.is_empty() {
            return;
        }
        if selected.iter().any(|row| *row < loaded) && !self.can_edit() {
            self.warning = Some(
                "Rows of a table without a primary key cannot be deleted here. Use SQL instead."
                    .into(),
            );
            return;
        }
        self.table_tabs[self.table_index]
            .edits
            .delete(loaded, selected);
        self.table_tabs[self.table_index].content.clear();
    }

    fn stage_insert(&mut self) {
        if self.busy.blocks(Lane::Data) {
            return;
        }
        if self.table_tabs[self.table_index]
            .structure
            .columns
            .is_empty()
            || !self.table_tabs[self.table_index]
                .selected_relation
                .as_ref()
                .is_some_and(Relation::is_table)
        {
            return;
        }
        let tab = &mut self.table_tabs[self.table_index];
        let index = tab.edits.add_row(tab.structure.columns.len());
        let row = self.table_tabs[self.table_index]
            .table_page
            .result
            .rows
            .len()
            + index;
        self.table_tabs[self.table_index]
            .content
            .selection
            .click(row, false);
        let first = self.table_tabs[self.table_index]
            .structure
            .columns
            .iter()
            .position(|c| !c.generated)
            .unwrap_or(0);
        self.table_tabs[self.table_index].content.cell = Some((row, first));
    }

    fn run_query(&mut self, scope: RunScope) {
        let document_id = self.documents[self.document_index].id;
        if self.busy.blocks(Lane::Sql(document_id))
            || self.connected.is_none()
            || self.servers_visible
            || self.mode != Mode::Query
        {
            return;
        }
        let document = &self.documents[self.document_index];
        let (sql, base) = match scope {
            RunScope::All => (document.sql.clone(), 0),
            RunScope::Statement => match self.current_statement() {
                Some(statement) => statement,
                None => return,
            },
        };
        if sql.trim().is_empty() {
            return;
        }
        let id = document.id;
        if self.send(Command::QueryIn(id, sql.clone()), "Executing SQL...") {
            self.running.insert(
                id,
                RunningQuery {
                    base,
                    prefix: 0,
                    kind: QueryKind::Normal,
                },
            );
            self.query_outputs.remove(&id);
            self.remember_query(sql);
        }
    }

    /// Adds SQL to the local history. Statements that may contain a password are
    /// never stored. A storage failure is reported without blocking execution.
    fn remember_query(&mut self, sql: String) {
        if lexer::contains_word(&sql, "password") {
            self.warning =
                Some("Not saved to query history because the SQL may contain a password.".into());
            return;
        }
        if self.history.first() != Some(&sql) {
            self.history.insert(0, sql);
            self.history.truncate(100);
        }
        if self.history_writable
            && let Some(store) = &self.store
            && let Err(error) = store.save_history(&self.history)
        {
            self.warning = Some(format!("Query history could not be saved: {error}"));
        }
    }

    fn cancel_current(&mut self) {
        let current = Lane::Sql(self.documents[self.document_index].id);
        let lane = if self.mode == Mode::Query && self.busy.blocks(current) && !self.busy.control {
            current
        } else if self.busy.data {
            Lane::Data
        } else if let Some(id) = self.busy.sql.iter().next() {
            Lane::Sql(*id)
        } else {
            return;
        };
        self.worker.cancel_lane(lane);
        self.status = "Cancelling...".into();
    }

    fn save_drafts(&mut self) {
        if !self.documents_writable {
            return;
        }
        if let Some(store) = &self.store {
            if let Err(error) = store.save_documents(&self.documents) {
                self.error = Some(error.to_string());
            } else {
                self.dirty_since = None;
            }
        }
    }

    fn select_document(&mut self, index: usize) {
        if index < self.documents.len() {
            self.document_index = index;
            self.mode = Mode::Query;
            self.editor_selection = 0..0;
            self.editor_cursor = 0;
            self.result_index = 0;
        }
    }

    fn new_document(&mut self) {
        let document = QueryDocument {
            title: next_query_title(&self.documents),
            sql: String::new(),
            ..Default::default()
        };
        self.documents.push(document);
        self.select_document(self.documents.len() - 1);
        self.save_drafts();
    }

    /// Swaps a document with its neighbour in the same folder.
    fn move_document(&mut self, index: usize, down: bool) {
        let folder = self.documents[index].folder.trim().to_owned();
        let neighbour = if down {
            (index + 1..self.documents.len()).find(|i| self.documents[*i].folder.trim() == folder)
        } else {
            (0..index)
                .rev()
                .find(|i| self.documents[*i].folder.trim() == folder)
        };
        if let Some(other) = neighbour {
            let current = self.documents[self.document_index].id;
            self.documents.swap(index, other);
            self.document_index = self
                .documents
                .iter()
                .position(|d| d.id == current)
                .unwrap_or(0);
            self.save_drafts();
        }
    }

    fn request_remove_document(&mut self, index: usize) {
        if self.busy.control {
            return;
        }
        let Some(document) = self.documents.get(index) else {
            return;
        };
        if self.running.contains_key(&document.id) {
            self.warning = Some("This query is running. Cancel it before removing it.".into());
            return;
        }
        if document.sql.trim().is_empty() {
            self.remove_document(document.id);
        } else {
            self.dialog = Some(Dialog::RemoveDocument(document.id));
        }
    }

    fn remove_document(&mut self, id: Uuid) {
        if self.busy.control || self.running.contains_key(&id) {
            return;
        }
        let Some(index) = self.documents.iter().position(|d| d.id == id) else {
            return;
        };
        let current = self.document_index;
        self.document_index = remove_document_at(&mut self.documents, current, index);
        self.query_outputs.remove(&id);
        self.transaction.remove(&id);
        if self.connected.is_some() {
            self.send(Command::CloseSession(id), "Query removed");
        }
        if index == current {
            self.editor_selection = 0..0;
            self.editor_cursor = 0;
            self.result_index = 0;
        }
        self.status = "Query removed".into();
        self.save_drafts();
    }

    fn open_sql(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("SQL", &["sql"])
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(sql) => {
                    let id = Uuid::new_v4();
                    self.remember_file(id, &path, &sql);
                    self.documents.push(QueryDocument {
                        id,
                        title: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into(),
                        sql,
                        path: Some(path),
                        folder: String::new(),
                    });
                    self.select_document(self.documents.len() - 1);
                    self.save_drafts();
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    fn save_sql(&mut self, choose_path: bool) {
        let document = &mut self.documents[self.document_index];
        let path = if choose_path || document.path.is_none() {
            rfd::FileDialog::new()
                .add_filter("SQL", &["sql"])
                .set_file_name(format!("{}.sql", document.title.trim_end_matches(".sql")))
                .save_file()
        } else {
            document.path.clone()
        };
        if let Some(path) = path {
            match storage::write_user_file(&path, document.sql.as_bytes()) {
                Ok(()) => {
                    document.title = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into();
                    document.path = Some(path.clone());
                    let (id, text) = (document.id, document.sql.clone());
                    self.remember_file(id, &path, &text);
                    self.status = "SQL file saved".into();
                    self.save_drafts();
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    /// Formats the selection, or the whole document when nothing is selected.
    fn format_sql(&mut self) {
        let document = &mut self.documents[self.document_index];
        let mut range = lexer::char_to_byte(&document.sql, self.editor_selection.start)
            ..lexer::char_to_byte(&document.sql, self.editor_selection.end);
        if range.is_empty() {
            range = 0..document.sql.len();
        }
        match sql::format_sql(&document.sql[range.clone()]) {
            Ok(formatted) => {
                document.sql.replace_range(range, &formatted);
                self.editor_selection = 0..0;
                self.dirty_since = Some(Instant::now());
            }
            Err(error) => self.warning = Some(error.to_string()),
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || self.error.is_some() {
            return;
        }
        let command = egui::Modifiers::COMMAND;
        let command_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        let bindings = [
            (command_shift, egui::Key::Enter, Action::RunAll),
            (command, egui::Key::Enter, Action::RunStatement),
            (command, egui::Key::Period, Action::Cancel),
            (command, egui::Key::T, Action::NewQuery),
            (command, egui::Key::O, Action::OpenFile),
            (command_shift, egui::Key::S, Action::SaveAs),
            (command, egui::Key::S, Action::Save),
            (command, egui::Key::W, Action::CloseQuery),
            (egui::Modifiers::CTRL, egui::Key::F, Action::Format),
            (command, egui::Key::F, Action::Find),
            (command, egui::Key::R, Action::Refresh),
            (command, egui::Key::E, Action::Explain),
            (command, egui::Key::OpenBracket, Action::Back),
        ];
        for (modifiers, key, action) in bindings {
            if ctx.input_mut(|i| i.consume_key(modifiers, key)) {
                self.perform(action);
            }
        }
    }

    /// Runs a command from a keyboard shortcut or the menu bar.
    fn perform(&mut self, action: Action) {
        if self.dialog.is_some() || self.error.is_some() {
            return;
        }
        let in_table = matches!(
            self.mode,
            Mode::Content | Mode::Structure | Mode::Definition | Mode::NewTable
        ) && !self.servers_visible;
        let in_query = self.mode == Mode::Query && !self.servers_visible;
        match action {
            Action::RunAll => self.run_query(RunScope::All),
            Action::RunStatement => self.run_query(RunScope::Statement),
            Action::Cancel => self.cancel_current(),
            _ if self.busy.control => {}
            Action::NewQuery => {
                self.new_document();
                if self.connected.is_some() {
                    self.servers_visible = false;
                }
            }
            Action::OpenFile => self.open_sql(),
            Action::Save if in_table => self.save_changes(),
            Action::Save if in_query => self.save_sql(false),
            Action::SaveAs if in_query => self.save_sql(true),
            Action::CloseQuery if in_table => {
                self.request_close_table(self.table_tabs[self.table_index].id)
            }
            Action::CloseQuery if in_query => self.request_remove_document(self.document_index),
            Action::Format if in_query => self.format_sql(),
            Action::Find if in_query => self.open_find(),
            Action::FindNext if in_query => self.pending_find_step = Some(true),
            Action::FindPrevious if in_query => self.pending_find_step = Some(false),
            Action::QueryHistory => self.show_history = !self.show_history,
            Action::Refresh => self.refresh(),
            Action::AddRow if self.mode == Mode::Content => self.stage_insert(),
            Action::DeleteRows if self.mode == Mode::Content => self.stage_delete(),
            Action::DiscardChanges => self.discard_changes(),
            Action::ToggleFilter if self.mode == Mode::Content => {
                self.table_tabs[self.table_index].show_filter =
                    !self.table_tabs[self.table_index].show_filter;
            }
            Action::ToggleDetails => {
                self.table_tabs[self.table_index].show_details =
                    !self.table_tabs[self.table_index].show_details
            }
            Action::Back => self.go_back(),
            Action::Servers if self.connected.is_some() => {
                self.servers_visible = !self.servers_visible;
            }
            Action::Disconnect if self.connected.is_some() => {
                if !self.blocked_by_edits() {
                    self.send(Command::Disconnect, "Disconnecting...");
                }
            }
            Action::Activity => self.open_activity(),
            Action::Roles => self.open_roles(),
            Action::Diagnostics => self.open_diagnostics(),
            Action::Explain if in_query => self.run_explain(false),
            Action::ExplainAnalyze if in_query => self.run_explain(true),
            Action::Appearance(appearance) => {
                self.ui_state.appearance = appearance;
                self.save_ui_state();
            }
            _ => {}
        }
    }
}

/// Commands available from shortcuts and the menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    NewQuery,
    OpenFile,
    Save,
    SaveAs,
    CloseQuery,
    RunStatement,
    RunAll,
    Cancel,
    Format,
    Find,
    FindNext,
    FindPrevious,
    QueryHistory,
    Refresh,
    AddRow,
    DeleteRows,
    DiscardChanges,
    ToggleFilter,
    ToggleDetails,
    Back,
    Servers,
    Disconnect,
    Activity,
    Roles,
    Diagnostics,
    Explain,
    ExplainAnalyze,
    Appearance(Appearance),
}

impl eframe::App for PosternApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync(
            &ctx,
            match self.ui_state.appearance {
                Appearance::System => egui::ThemePreference::System,
                Appearance::Light => egui::ThemePreference::Light,
                Appearance::Dark => egui::ThemePreference::Dark,
            },
        );
        if ctx.input(|i| i.viewport().close_requested())
            && self.table_tabs.iter().any(TableTab::has_changes)
            && !self.allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::QuitWithChanges);
        }
        self.receive();
        #[cfg(target_os = "macos")]
        for action in self
            .menu
            .as_ref()
            .map(|menu| menu.poll())
            .unwrap_or_default()
        {
            self.perform(action);
        }
        #[cfg(feature = "smoke")]
        self.tick_smoke(&ctx);
        self.shortcuts(&ctx);
        self.sync_files();
        ctx.request_repaint_after(Duration::from_secs(2));
        if self
            .dirty_since
            .is_some_and(|time| time.elapsed() > Duration::from_secs(2))
        {
            self.save_drafts();
        }
        if self.dirty_since.is_some() {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        egui::Panel::top("toolbar")
            .exact_size(60.0)
            .frame(egui::Frame::new().fill(theme::toolbar()).inner_margin(12))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .exact_size(28.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::toolbar())
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.busy.any() {
                        ui.spinner();
                    }
                    ui.label(RichText::new(&self.status).small().color(theme::muted()));
                    if let Some(warning) = self.warning.clone() {
                        ui.separator();
                        ui.label(RichText::new(&warning).small().color(theme::gold()))
                            .on_hover_text(&warning);
                        if ui.small_button("Dismiss").clicked() {
                            self.warning = None;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Postern 0.1").small().color(theme::muted()));
                        let before = self.ui_state.appearance;
                        egui::ComboBox::from_id_salt("appearance")
                            .selected_text(RichText::new(before.label()).small())
                            .show_ui(ui, |ui| {
                                for appearance in Appearance::ALL {
                                    ui.selectable_value(
                                        &mut self.ui_state.appearance,
                                        appearance,
                                        appearance.label(),
                                    );
                                }
                            });
                        if self.ui_state.appearance != before {
                            self.save_ui_state();
                        }
                    });
                });
            });
        if self.servers_visible {
            self.servers(ui);
        } else {
            self.workspace(ui);
        }
        self.dialogs(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_drafts();
    }
}

/// Returns a "Query N" title numbered after the highest existing one, so titles
/// stay unique after documents are removed.
fn next_query_title(documents: &[QueryDocument]) -> String {
    let highest = documents
        .iter()
        .filter_map(|d| d.title.strip_prefix("Query ")?.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    format!("Query {}", highest.saturating_add(1))
}

/// Removes the document at `index` and returns the index of the document to show.
/// The list never becomes empty because the UI always renders a current document.
fn remove_document_at(documents: &mut Vec<QueryDocument>, current: usize, index: usize) -> usize {
    if index >= documents.len() {
        return current.min(documents.len().saturating_sub(1));
    }
    documents.remove(index);
    if documents.is_empty() {
        documents.push(QueryDocument {
            title: next_query_title(documents),
            sql: String::new(),
            ..Default::default()
        });
        return 0;
    }
    let current = if index < current {
        current - 1
    } else {
        current
    };
    current.min(documents.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn documents(titles: &[&str]) -> Vec<QueryDocument> {
        titles
            .iter()
            .map(|title| QueryDocument {
                title: (*title).into(),
                ..Default::default()
            })
            .collect()
    }

    fn titles(documents: &[QueryDocument]) -> Vec<&str> {
        documents.iter().map(|d| d.title.as_str()).collect()
    }

    #[test]
    fn removing_a_document_keeps_a_valid_current_index() {
        let mut list = documents(&["a", "b", "c", "d"]);
        assert_eq!(remove_document_at(&mut list, 2, 0), 1);
        assert_eq!(titles(&list), ["b", "c", "d"]);
        assert_eq!(remove_document_at(&mut list, 1, 1), 1);
        assert_eq!(titles(&list), ["b", "d"]);
        assert_eq!(remove_document_at(&mut list, 0, 1), 0);
        assert_eq!(titles(&list), ["b"]);
        let mut list = documents(&["a", "b"]);
        assert_eq!(remove_document_at(&mut list, 1, 1), 0);
        assert_eq!(titles(&list), ["a"]);
        assert_eq!(remove_document_at(&mut list, 0, 5), 0);
        assert_eq!(titles(&list), ["a"]);
    }

    #[test]
    fn removing_the_last_document_leaves_an_empty_query() {
        let mut list = documents(&["Query 7"]);
        assert_eq!(remove_document_at(&mut list, 0, 0), 0);
        assert_eq!(titles(&list), ["Query 1"]);
        assert!(list[0].sql.is_empty());
        assert!(list[0].path.is_none());
    }

    #[test]
    fn new_query_titles_follow_the_highest_number() {
        assert_eq!(next_query_title(&[]), "Query 1");
        assert_eq!(next_query_title(&documents(&["SQL Query"])), "Query 1");
        assert_eq!(
            next_query_title(&documents(&["Query 1", "Query 3", "report.sql"])),
            "Query 4"
        );
        assert_eq!(
            next_query_title(&documents(&["Query x", "Query"])),
            "Query 1"
        );
    }
}
