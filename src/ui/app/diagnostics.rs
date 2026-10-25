//! Database diagnostics: the index advisor and the slowest statements from
//! `pg_stat_statements`. Findings come with SQL that opens in a new editor
//! document for review; nothing runs without the user executing it there. The
//! only server changes made from here are the pg_stat_statements setup steps,
//! each started by its own button, and the restart asks for confirmation.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum DiagnosticsTab {
    #[default]
    Indexes,
    Queries,
}

/// Something the user chose in the diagnostics view, applied after drawing.
enum Choice {
    Refresh,
    OpenTable(String, String),
    OpenSql {
        title: String,
        sql: String,
    },
    Explain {
        title: String,
        sql: String,
    },
    EnableStatements,
    ConfigureStatements,
    SetUpStatements {
        data_directory: String,
        log_file: String,
        other_sessions: i64,
        configure: bool,
    },
}

impl PosternApp {
    pub(super) fn open_diagnostics(&mut self) {
        if self.connected.is_none() || self.blocked_by_edits() {
            return;
        }
        self.mode = Mode::Diagnostics;
        let loaded = match self.diagnostics_tab {
            DiagnosticsTab::Indexes => self.index_report.is_some(),
            DiagnosticsTab::Queries => self.slow_queries.is_some(),
        };
        if !loaded {
            self.refresh_diagnostics();
        }
    }

    pub(super) fn refresh_diagnostics(&mut self) {
        match self.diagnostics_tab {
            DiagnosticsTab::Indexes => {
                self.send(Command::IndexReport, "Checking indexes...");
            }
            DiagnosticsTab::Queries => {
                self.send(
                    Command::SlowQueries(self.slow_request),
                    "Loading query statistics...",
                );
            }
        }
    }

    pub(super) fn diagnostics_view(&mut self, ui: &mut egui::Ui) {
        let data_busy = self.busy.blocks(Lane::Data);
        let mut choice = None;
        egui::Panel::top("diagnostics_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::background())
                    .inner_margin(10),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Diagnostics");
                    ui.add_space(12.0);
                    for (tab, label) in [
                        (DiagnosticsTab::Indexes, "Index advisor"),
                        (DiagnosticsTab::Queries, "Slow queries"),
                    ] {
                        if ui
                            .selectable_label(self.diagnostics_tab == tab, label)
                            .clicked()
                            && self.diagnostics_tab != tab
                        {
                            self.diagnostics_tab = tab;
                            let loaded = match tab {
                                DiagnosticsTab::Indexes => self.index_report.is_some(),
                                DiagnosticsTab::Queries => self.slow_queries.is_some(),
                            };
                            if !loaded {
                                choice = Some(Choice::Refresh);
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!data_busy, egui::Button::new("Refresh"))
                            .on_hover_text("Run the checks again (Command-R)")
                            .clicked()
                        {
                            choice = Some(Choice::Refresh);
                        }
                        if self.diagnostics_tab == DiagnosticsTab::Queries
                            && self
                                .slow_queries
                                .as_ref()
                                .is_some_and(|r| r.status == StatementsStatus::Ready)
                            && self.slow_query_controls(ui, data_busy)
                        {
                            choice = Some(Choice::Refresh);
                        }
                    });
                });
            });
        egui::ScrollArea::both()
            .id_salt("diagnostics_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(12).show(ui, |ui| {
                    let picked = match self.diagnostics_tab {
                        DiagnosticsTab::Indexes => self.index_advisor(ui),
                        DiagnosticsTab::Queries => self.slow_query_list(ui, data_busy),
                    };
                    if picked.is_some() {
                        choice = picked;
                    }
                });
            });
        match choice {
            Some(Choice::Refresh) => self.refresh_diagnostics(),
            Some(Choice::OpenTable(schema, name)) => {
                if let Some(relation) = self
                    .relations
                    .iter()
                    .find(|r| r.schema == schema && r.name == name)
                    .cloned()
                {
                    self.select_relation(relation);
                }
            }
            Some(Choice::OpenSql { title, sql }) => self.open_in_editor(title, sql),
            Some(Choice::Explain { title, sql }) => self.explain_recorded(title, sql),
            Some(Choice::EnableStatements) => {
                self.send(
                    Command::EnableQueryStats(self.slow_request),
                    "Installing pg_stat_statements...",
                );
            }
            Some(Choice::ConfigureStatements) => {
                self.send(
                    Command::ConfigureQueryStats(self.slow_request),
                    "Configuring shared_preload_libraries...",
                );
            }
            Some(Choice::SetUpStatements {
                data_directory,
                log_file,
                other_sessions,
                configure,
            }) if !self.blocked_by_edits() => {
                self.dialog = Some(Dialog::SetUpQueryStats {
                    data_directory,
                    log_file,
                    other_sessions,
                    configure,
                });
            }
            Some(Choice::SetUpStatements { .. }) => {}
            None => {}
        }
    }

    /// Opens a recorded statement in a new SQL document and explains it, with a
    /// generic plan when it has `$n` parameters.
    pub(super) fn explain_recorded(&mut self, title: String, sql: String) {
        self.open_in_editor(title, sql);
        self.run_explain(false);
    }

    /// Ordering, scope, and reset controls. Returns true when the list must reload.
    fn slow_query_controls(&mut self, ui: &mut egui::Ui, data_busy: bool) -> bool {
        let before = self.slow_request;
        if ui
            .add_enabled(!data_busy, egui::Button::new("Reset statistics..."))
            .on_hover_text("Clear pg_stat_statements for the whole server")
            .clicked()
        {
            self.dialog = Some(Dialog::ResetQueryStats);
        }
        ui.add_enabled_ui(!data_busy, |ui| {
            ui.checkbox(&mut self.slow_request.include_catalog, "Catalog queries")
                .on_hover_text(
                    "Include statements that read pg_catalog or information_schema, such as the catalog queries of database clients like this one",
                );
            ui.checkbox(&mut self.slow_request.all_databases, "All databases");
            egui::ComboBox::from_id_salt("slow_query_order")
                .width(100.0)
                .selected_text(self.slow_request.order.label())
                .show_ui(ui, |ui| {
                    for order in SlowQueryOrder::ALL {
                        ui.selectable_value(&mut self.slow_request.order, order, order.label());
                    }
                });
            ui.label("Order by");
        });
        self.slow_request != before
    }

    fn index_advisor(&self, ui: &mut egui::Ui) -> Option<Choice> {
        let Some(report) = &self.index_report else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking indexes...");
            });
            return None;
        };
        let mut choice = None;
        if report.items.is_empty() {
            ui.label(
                RichText::new("No index or table problems found.")
                    .strong()
                    .color(theme::green()),
            );
        } else {
            let reclaimable: i64 = report
                .items
                .iter()
                .filter(|item| {
                    matches!(
                        item.kind,
                        AdviceKind::InvalidIndex
                            | AdviceKind::DuplicateIndex
                            | AdviceKind::RedundantIndex
                            | AdviceKind::UnusedIndex
                    )
                })
                .filter_map(|item| item.bytes)
                .sum();
            let mut summary = format!("{} findings.", report.items.len());
            if reclaimable > 0 {
                summary.push_str(&format!(
                    " Dropping the listed indexes would free about {}.",
                    structure::bytes(reclaimable)
                ));
            }
            ui.label(RichText::new(summary).strong());
        }
        ui.label(
            RichText::new(match &report.stats_since {
                Some(since) => format!("Usage statistics since {since}."),
                None => "Usage statistics have been collected since the server's statistics were created.".into(),
            })
            .small()
            .color(theme::muted()),
        );
        ui.label(
            RichText::new("Fixes open in a new SQL document for review. CONCURRENTLY statements cannot run inside a transaction.")
                .small()
                .color(theme::muted()),
        );
        ui.add_space(8.0);
        for kind in AdviceKind::ALL {
            let items: Vec<&IndexAdvice> = report
                .items
                .iter()
                .filter(|item| item.kind == kind)
                .collect();
            if items.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(
                RichText::new(format!("{} ({})", kind.title(), items.len())).strong(),
            )
            .id_salt(("advice", kind))
            .default_open(true)
            .show(ui, |ui| {
                ui.label(RichText::new(kind.explanation()).color(theme::muted()));
                ui.add_space(4.0);
                egui::Grid::new(("advice_grid", kind))
                    .striped(true)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        for title in [
                            "Table",
                            "Index or constraint",
                            "Size",
                            "",
                            "Details and fix",
                        ] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for item in items {
                            if ui
                                .link(format!("{}.{}", item.schema, item.table))
                                .on_hover_text("Open the table")
                                .clicked()
                            {
                                choice = Some(Choice::OpenTable(
                                    item.schema.clone(),
                                    item.table.clone(),
                                ));
                            }
                            ui.label(RichText::new(&item.object).monospace());
                            ui.label(item.bytes.map(structure::bytes).unwrap_or_default());
                            ui.horizontal(|ui| {
                                let Some(fix) = &item.fix else {
                                    return;
                                };
                                if ui
                                    .small_button("Open in SQL editor")
                                    .on_hover_text("Review and run the fix in a new SQL document")
                                    .clicked()
                                {
                                    choice = Some(Choice::OpenSql {
                                        title: format!(
                                            "Fix {}",
                                            if item.object.is_empty() {
                                                &item.table
                                            } else {
                                                &item.object
                                            }
                                        ),
                                        sql: fix_document(item),
                                    });
                                }
                                if ui.small_button("Copy").clicked() {
                                    ui.ctx().copy_text(format!("{fix};"));
                                }
                            });
                            ui.vertical(|ui| {
                                ui.label(&item.detail);
                                if let Some(fix) = &item.fix {
                                    clipped(
                                        ui,
                                        RichText::new(fix).monospace().color(theme::muted()),
                                        520.0,
                                    )
                                    .on_hover_text(RichText::new(fix).monospace());
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(6.0);
        }
        choice
    }

    fn slow_query_list(&self, ui: &mut egui::Ui, data_busy: bool) -> Option<Choice> {
        let Some(report) = &self.slow_queries else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading query statistics...");
            });
            return None;
        };
        let mut choice = None;
        match &report.status {
            StatementsStatus::Setup(setup) => {
                return self.statements_setup(ui, setup, data_busy);
            }
            StatementsStatus::Unavailable(message) => {
                ui.label(RichText::new("Query statistics are not available.").strong());
                ui.label(RichText::new(message).color(theme::gold()));
                return choice;
            }
            StatementsStatus::Ready => {}
        }
        let mut summary = format!("{} statements", report.queries.len());
        if let Some(since) = &report.stats_since {
            summary.push_str(&format!(" · collected since {since}"));
        }
        ui.label(RichText::new(summary).color(theme::muted()));
        if report.queries.is_empty() {
            ui.add_space(8.0);
            ui.label("No statements have been recorded yet.");
            return choice;
        }
        ui.add_space(6.0);
        let all = self.slow_request.all_databases;
        egui::Grid::new("slow_queries")
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                let mut titles = vec!["Total", "Share", "Calls", "Mean", "Max", "Rows", "Cache hits"];
                if all {
                    titles.extend(["User", "Database"]);
                }
                titles.extend(["", "Query"]);
                for title in titles {
                    ui.strong(title);
                }
                ui.end_row();
                for (index, query) in report.queries.iter().enumerate() {
                    ui.label(RichText::new(milliseconds(query.total_ms)).strong());
                    ui.add(
                        egui::ProgressBar::new((query.share / 100.0).clamp(0.0, 1.0) as f32)
                            .desired_width(70.0)
                            .corner_radius(2.0)
                            .text(format!("{:.1}%", query.share)),
                    )
                    .on_hover_text("Share of the total execution time of all recorded statements");
                    ui.label(table::group(query.calls));
                    ui.label(if query.mean_ms >= 1000.0 {
                        RichText::new(milliseconds(query.mean_ms)).color(theme::gold())
                    } else {
                        RichText::new(milliseconds(query.mean_ms))
                    });
                    ui.label(milliseconds(query.max_ms));
                    ui.label(table::group(query.rows));
                    ui.label(match query.hit_ratio {
                        Some(ratio) if ratio < 0.9 => {
                            RichText::new(format!("{:.1}%", ratio * 100.0)).color(theme::gold())
                        }
                        Some(ratio) => RichText::new(format!("{:.1}%", ratio * 100.0)),
                        None => RichText::new("—").color(theme::muted()),
                    })
                    .on_hover_text("Shared buffer reads served from cache");
                    if all {
                        ui.label(&query.user);
                        ui.label(&query.database);
                    }
                    ui.horizontal(|ui| {
                        let title = format!("Slow query {}", index + 1);
                        let explainable = explainable(&query.query)
                            && (report.generic_plans || !lexer::has_parameters(&query.query));
                        if ui
                            .small_button("Open")
                            .on_hover_text("Open in a new SQL document")
                            .clicked()
                        {
                            choice = Some(Choice::OpenSql {
                                title: title.clone(),
                                sql: query_document(query),
                            });
                        }
                        if ui
                            .add_enabled(explainable, egui::Button::new("Explain").small())
                            .on_hover_text(if report.generic_plans {
                                "Open in a new SQL document and show its plan. Statements with $1 parameters get a generic plan."
                            } else {
                                "Open in a new SQL document and show its plan. Generic plans for statements with $1 parameters need PostgreSQL 16."
                            })
                            .clicked()
                        {
                            choice = Some(Choice::Explain {
                                title,
                                sql: query_document(query),
                            });
                        }
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(query.query.clone());
                        }
                    });
                    let text: String = query.query.split_whitespace().collect::<Vec<_>>().join(" ");
                    let hover: String = query.query.chars().take(4000).collect();
                    clipped(ui, RichText::new(&text).monospace(), 520.0)
                        .on_hover_text(RichText::new(hover).monospace());
                    ui.end_row();
                }
            });
        choice
    }
}

impl PosternApp {
    /// The steps pg_stat_statements still needs, with a button for the next one
    /// Postern can take.
    fn statements_setup(
        &self,
        ui: &mut egui::Ui,
        setup: &StatementsSetup,
        data_busy: bool,
    ) -> Option<Choice> {
        let mut choice = None;
        let profile = self.connected.as_ref();
        let database = profile.map(|p| p.database.as_str()).unwrap_or_default();
        let read_only = profile.is_some_and(|p| p.read_only);
        let enabled = !data_busy && !self.busy.control && !read_only;
        ui.label(RichText::new("Query statistics need the pg_stat_statements extension.").strong());
        ui.label(
            RichText::new("PostgreSQL records statements only when the server loads the extension's library at startup.")
                .color(theme::muted()),
        );
        ui.add_space(10.0);
        egui::Grid::new("statements_setup")
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                let mut step = |done: bool, title: String, note: &str| {
                    ui.label(if done {
                        RichText::new("Done").strong().color(theme::green())
                    } else {
                        RichText::new("Needed").strong().color(theme::gold())
                    });
                    ui.label(title);
                    ui.label(RichText::new(note).small().color(theme::muted()));
                    ui.end_row();
                };
                step(
                    setup.configured || setup.preloaded,
                    "Load pg_stat_statements when the server starts".into(),
                    if setup.configured && !setup.preloaded {
                        "Configured; takes effect after a restart."
                    } else {
                        "shared_preload_libraries"
                    },
                );
                step(setup.preloaded, "Restart the server".into(), "");
                step(
                    setup.installed,
                    format!("Create the extension in {database}"),
                    "CREATE EXTENSION pg_stat_statements",
                );
            });
        ui.add_space(10.0);
        if read_only {
            ui.label(
                RichText::new(
                    "This connection is read-only, so Postern does not change the server.",
                )
                .color(theme::gold()),
            );
        }
        if setup.preloaded {
            if ui
                .add_enabled(enabled, egui::Button::new("Install pg_stat_statements"))
                .on_hover_text("CREATE EXTENSION pg_stat_statements")
                .clicked()
            {
                choice = Some(Choice::EnableStatements);
            }
            return choice;
        }
        match &setup.restart {
            RestartSupport::Available {
                data_directory,
                log_file,
                other_sessions,
            } if setup.configure_blocker.is_none() => {
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(
                            RichText::new("Set up and restart server...")
                                .color(egui::Color32::WHITE),
                        )
                        .fill(theme::accent()),
                    )
                    .on_hover_text(
                        "Configure, restart, and install in one step, after a confirmation",
                    )
                    .clicked()
                {
                    choice = Some(Choice::SetUpStatements {
                        data_directory: data_directory.clone(),
                        log_file: log_file.clone(),
                        other_sessions: *other_sessions,
                        configure: !setup.configured,
                    });
                }
                ui.label(
                    RichText::new(format!("The server at {data_directory} runs on this Mac, so Postern can restart it."))
                        .small()
                        .color(theme::muted()),
                );
            }
            restart => {
                if let Some(blocker) = &setup.configure_blocker {
                    ui.label(RichText::new(blocker).color(theme::gold()));
                } else if !setup.configured
                    && ui
                        .add_enabled(enabled, egui::Button::new("Add to shared_preload_libraries"))
                        .on_hover_text("ALTER SYSTEM SET shared_preload_libraries, keeping the libraries already configured")
                        .clicked()
                {
                    choice = Some(Choice::ConfigureStatements);
                }
                if let RestartSupport::Unavailable(reason) = restart {
                    ui.label(RichText::new(reason).color(theme::muted()));
                }
                ui.label(
                    "Restart the server with the tool that manages it, such as Postgres.app, brew services, systemctl, or your provider's console. Then choose Refresh.",
                );
                if setup.configure_blocker.is_some() {
                    ui.add_space(6.0);
                    let manual = "shared_preload_libraries = 'pg_stat_statements'\n\n-- after the restart\nCREATE EXTENSION pg_stat_statements;";
                    egui::Frame::new()
                        .fill(theme::toolbar())
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.label(RichText::new(manual).monospace());
                        });
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(manual.to_owned());
                    }
                    ui.label(
                        RichText::new(
                            "Keep any libraries already listed in shared_preload_libraries.",
                        )
                        .small()
                        .color(theme::muted()),
                    );
                }
            }
        }
        choice
    }
}

/// A single left-aligned line clipped to `width`, with an ellipsis.
fn clipped(ui: &mut egui::Ui, text: RichText, width: f32) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

/// An editor document for an advisor fix, with the finding as a comment.
fn fix_document(item: &IndexAdvice) -> String {
    let target = if item.object.is_empty() {
        format!("{}.{}", item.schema, item.table)
    } else {
        format!("{} on {}.{}", item.object, item.schema, item.table)
    };
    format!(
        "-- {}: {target}\n-- {}\n{};\n",
        item.kind.title(),
        item.detail.replace('\n', " "),
        item.fix.as_deref().unwrap_or_default()
    )
}

/// An editor document for a recorded statement, with its statistics as a comment.
fn query_document(query: &SlowQuery) -> String {
    format!(
        "-- {} calls, {} total, {} mean, {} max\n{};\n",
        query.calls,
        milliseconds(query.total_ms),
        milliseconds(query.mean_ms),
        milliseconds(query.max_ms),
        query.query.trim().trim_end_matches(';')
    )
}

/// Statements EXPLAIN accepts.
fn explainable(sql: &str) -> bool {
    matches!(
        lexer::first_word(sql).as_deref(),
        Some("SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "WITH" | "VALUES" | "TABLE")
    )
}

/// A duration in milliseconds, in the largest unit that keeps it readable.
fn milliseconds(ms: f64) -> String {
    let ms = ms.max(0.0);
    if ms < 1.0 {
        format!("{ms:.3} ms")
    } else if ms < 1000.0 {
        format!("{ms:.1} ms")
    } else if ms < 60_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms < 3_600_000.0 {
        let seconds = (ms / 1000.0).round() as i64;
        format!("{} min {} s", seconds / 60, seconds % 60)
    } else {
        format!("{:.1} h", ms / 3_600_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(milliseconds(0.0421), "0.042 ms");
        assert_eq!(milliseconds(12.34), "12.3 ms");
        assert_eq!(milliseconds(4321.0), "4.32 s");
        assert_eq!(milliseconds(125_400.0), "2 min 5 s");
        assert_eq!(milliseconds(5_400_000.0), "1.5 h");
    }

    #[test]
    fn fix_documents_explain_the_finding() {
        let item = IndexAdvice {
            kind: AdviceKind::UnusedIndex,
            schema: "public".into(),
            table: "orders".into(),
            object: "orders_note".into(),
            detail: "No index scans.".into(),
            bytes: Some(8192),
            fix: Some(r#"DROP INDEX CONCURRENTLY "public"."orders_note""#.into()),
        };
        assert_eq!(
            fix_document(&item),
            "-- Unused indexes: orders_note on public.orders\n-- No index scans.\nDROP INDEX CONCURRENTLY \"public\".\"orders_note\";\n"
        );
        let query = SlowQuery {
            query: "SELECT * FROM orders WHERE id = $1".into(),
            calls: 3,
            total_ms: 30.0,
            mean_ms: 10.0,
            max_ms: 12.0,
            ..Default::default()
        };
        assert_eq!(
            query_document(&query),
            "-- 3 calls, 30.0 ms total, 10.0 ms mean, 12.0 ms max\nSELECT * FROM orders WHERE id = $1;\n"
        );
        assert!(explainable(&query.query));
        assert!(explainable("with x as (select 1) select * from x"));
        assert!(!explainable("VACUUM orders"));
        assert!(!explainable("BEGIN"));
    }
}
