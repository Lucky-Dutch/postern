use super::*;
use crate::ui::grid::{CopyFormat, GridSource};

/// What the query list asked for.
enum DocumentRequest {
    Select(usize),
    Remove(usize),
    /// Move a document up or down (true) within its folder.
    Move(usize, bool),
    Duplicate(usize),
    Folder(usize),
}

impl PosternApp {
    pub(super) fn workspace(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("navigator")
            .default_size(250.0)
            .min_size(190.0)
            .max_size(420.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(theme::sidebar()).inner_margin(12))
            .show(ui, |ui| self.navigator(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::background()))
            .show(ui, |ui| {
                self.table_tabs_bar(ui);
                match self.mode {
                    Mode::Query => self.query_workspace(ui),
                    Mode::Routine => self.routine_view(ui),
                    Mode::Activity => self.activity_view(ui),
                    Mode::Roles => self.roles_view(ui),
                    Mode::Diagnostics => self.diagnostics_view(ui),
                    Mode::Content | Mode::Structure | Mode::Definition | Mode::NewTable => {
                        let id = self.table_tabs[self.table_index].id;
                        ui.push_id(id, |ui| {
                            ui.add_enabled_ui(self.ddl_tab != Some(id), |ui| {
                                if self.mode == Mode::NewTable {
                                    self.table_editor_view(ui);
                                } else {
                                    self.table_workspace(ui);
                                }
                            });
                        });
                    }
                }
            });
    }

    fn navigator(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Queries");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!self.busy.control, |ui| {
                    if ui
                        .add_enabled(self.mode == Mode::Query, egui::Button::new("−").small())
                        .on_hover_text("Remove the selected query (Command-W)")
                        .clicked()
                    {
                        self.request_remove_document(self.document_index);
                    }
                    if ui
                        .small_button("+")
                        .on_hover_text("New query (Command-T)")
                        .clicked()
                    {
                        self.new_document();
                    }
                    if ui.small_button("Open").clicked() {
                        self.open_sql();
                    }
                });
            });
        });
        ui.add_space(4.0);
        let mut request = None;
        let mut folders: Vec<&str> = self.documents.iter().map(|d| d.folder.trim()).collect();
        folders.sort_unstable();
        folders.dedup();
        egui::ScrollArea::vertical()
            .id_salt("query_list")
            .max_height(220.0)
            .show(ui, |ui| {
                ui.add_enabled_ui(!self.busy.control, |ui| {
                    for folder in &folders {
                        let mut items = |ui: &mut egui::Ui| {
                            for (index, document) in self
                                .documents
                                .iter()
                                .enumerate()
                                .filter(|(_, d)| d.folder.trim() == *folder)
                            {
                                let running = self.running.contains_key(&document.id);
                                let mut title = document.title.clone();
                                if self.unsaved(document) {
                                    title.push_str(" •");
                                }
                                if running {
                                    title.push_str(" …");
                                }
                                let response = theme::navigation_item(
                                    ui,
                                    &title,
                                    self.mode == Mode::Query && index == self.document_index,
                                    "sql",
                                );
                                let response = match &document.path {
                                    Some(path) => {
                                        response.on_hover_text(path.display().to_string())
                                    }
                                    None => response,
                                };
                                if response.clicked() {
                                    request = Some(DocumentRequest::Select(index));
                                }
                                response.context_menu(|ui| {
                                    for (label, value) in [
                                        ("Move up", DocumentRequest::Move(index, false)),
                                        ("Move down", DocumentRequest::Move(index, true)),
                                        ("Duplicate", DocumentRequest::Duplicate(index)),
                                        ("Move to folder...", DocumentRequest::Folder(index)),
                                    ] {
                                        if ui.button(label).clicked() {
                                            request = Some(value);
                                            ui.close();
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("Remove query...").clicked() {
                                        request = Some(DocumentRequest::Remove(index));
                                        ui.close();
                                    }
                                });
                            }
                        };
                        if folder.is_empty() {
                            items(ui);
                        } else {
                            egui::CollapsingHeader::new(
                                RichText::new(*folder).color(theme::muted()),
                            )
                            .id_salt(("query_folder", *folder))
                            .default_open(true)
                            .show(ui, items);
                        }
                    }
                });
            });
        match request {
            Some(DocumentRequest::Select(index)) => self.select_document(index),
            Some(DocumentRequest::Remove(index)) => self.request_remove_document(index),
            Some(DocumentRequest::Move(index, down)) => self.move_document(index, down),
            Some(DocumentRequest::Duplicate(index)) => {
                let source = &self.documents[index];
                let (title, sql, folder) = (
                    format!("{} copy", source.title),
                    source.sql.clone(),
                    source.folder.clone(),
                );
                self.open_in_editor(title, sql);
                if let Some(document) = self.documents.last_mut() {
                    document.folder = folder;
                }
            }
            Some(DocumentRequest::Folder(index)) => {
                self.dialog = Some(Dialog::MoveToFolder {
                    document: self.documents[index].id,
                    folder: self.documents[index].folder.clone(),
                });
            }
            None => {}
        }
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
        self.object_list(ui);
    }

    fn query_workspace(&mut self, ui: &mut egui::Ui) {
        let result_height = (ui.available_height() * 0.42).max(180.0);
        egui::Panel::bottom("query_results")
            .default_size(result_height)
            .min_size(120.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(theme::background()))
            .show(ui, |ui| self.query_results(ui));
        let current = self.documents[self.document_index].id;
        let sql_busy = self.busy.blocks(Lane::Sql(current));
        egui::Panel::bottom("execute_bar")
            .exact_size(48.0)
            .frame(egui::Frame::new().fill(theme::sidebar()).inner_margin(9))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Query History").clicked() {
                        self.show_history = !self.show_history;
                    }
                    match self.transaction.get(&current).copied().unwrap_or_default() {
                        TransactionState::Open => {
                            ui.label(RichText::new("● Transaction open").color(theme::gold()))
                                .on_hover_text("Run COMMIT or ROLLBACK to finish the transaction.");
                        }
                        TransactionState::Failed => {
                            ui.label(
                                RichText::new("● Transaction failed")
                                    .color(egui::Color32::from_rgb(223, 113, 111)),
                            )
                            .on_hover_text("The transaction is aborted. Run ROLLBACK.");
                        }
                        TransactionState::Idle | TransactionState::Unknown => {}
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!sql_busy && self.connected.is_some(), |ui| {
                            ui.menu_button("Explain", |ui| {
                                if ui
                                    .button("Explain (Command-E)")
                                    .on_hover_text("Show the plan without running the statement")
                                    .clicked()
                                {
                                    self.run_explain(false);
                                    ui.close();
                                }
                                if ui
                                    .button("Explain Analyze")
                                    .on_hover_text("Run the statement in a transaction that is rolled back, and show measured timings")
                                    .clicked()
                                {
                                    self.run_explain(true);
                                    ui.close();
                                }
                            });
                            if ui
                                .button("Run All")
                                .on_hover_text("Run the whole document (Command-Shift-Return)")
                                .clicked()
                            {
                                self.run_query(RunScope::All);
                            }
                            let label = if self.editor_selection.is_empty() {
                                "Run Statement"
                            } else {
                                "Run Selection"
                            };
                            if theme::primary(ui, label)
                                .on_hover_text("Run the selection, or the statement at the cursor (Command-Return)")
                                .clicked()
                            {
                                self.run_query(RunScope::Statement);
                            }
                        });
                        if ui
                            .add_enabled(self.busy.sql.contains(&current), egui::Button::new("Cancel"))
                            .on_hover_text("Cancel the running query (Command-.)")
                            .clicked()
                        {
                            self.worker.cancel_lane(Lane::Sql(current));
                            self.status = "Cancelling query...".into();
                        }
                        let timeout = self
                            .connected
                            .as_ref()
                            .map_or(DEFAULT_STATEMENT_TIMEOUT_SECS, |p| p.statement_timeout_secs);
                        ui.label(
                            RichText::new(if timeout == 0 {
                                "No statement timeout".to_owned()
                            } else {
                                format!("{timeout}s per statement")
                            })
                            .small()
                            .color(theme::muted()),
                        );
                    });
                });
            });
        egui::Panel::top("query_document_bar")
            .exact_size(48.0)
            .frame(egui::Frame::new().fill(theme::background()).inner_margin(9))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(
                                &mut self.documents[self.document_index].title,
                            )
                            .desired_width(180.0),
                        )
                        .changed()
                    {
                        self.dirty_since = Some(Instant::now());
                    }
                    if ui
                        .button("Save")
                        .on_hover_text("Save to a SQL file (Command-S)")
                        .clicked()
                    {
                        self.save_sql(false);
                    }
                    if ui.button("Save as...").clicked() {
                        self.save_sql(true);
                    }
                    if ui
                        .button("Format")
                        .on_hover_text("Format the selection or the whole document (Control-F)")
                        .clicked()
                    {
                        self.format_sql();
                    }
                    if self.documents[self.document_index].path.is_some()
                        && ui
                            .button("Reload file")
                            .on_hover_text("Replace the current editor text with the file on disk")
                            .clicked()
                    {
                        let path = self.documents[self.document_index].path.clone().unwrap();
                        match std::fs::read_to_string(&path) {
                            Ok(sql) => {
                                let id = self.documents[self.document_index].id;
                                self.remember_file(id, &path, &sql);
                                self.documents[self.document_index].sql = sql;
                                self.editor_selection = 0..0;
                                self.dirty_since = Some(Instant::now());
                            }
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                });
            });
        self.editor(ui);
    }

    fn query_results(&mut self, ui: &mut egui::Ui) {
        let id = self.documents[self.document_index].id;
        let running = self.running.contains_key(&id);
        let Some(output) = self.query_outputs.get(&id) else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(if running {
                        "Executing query..."
                    } else {
                        "Results appear here. Run the statement at the cursor with Command-Return."
                    })
                    .color(theme::muted()),
                );
            });
            return;
        };
        let mut export = None;
        let mut export_all = None;
        let mut show_error = None;
        let mut inspect = None;
        let mut copied = false;
        egui::Panel::top("result_tabs")
            .frame(egui::Frame::new().fill(theme::toolbar()).inner_margin(7))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (index, result) in output.results.iter().enumerate() {
                        let rows = if result.columns.is_empty() {
                            format!("{} affected", result.affected)
                        } else {
                            format!("{} rows", result.affected)
                        };
                        let command = if result.command.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", result.command)
                        };
                        ui.selectable_value(
                            &mut self.result_index,
                            index,
                            format!("{}{command} · {rows}", index + 1),
                        );
                    }
                    if let Some(result) = output.results.get(self.result_index) {
                        if ui.button("Copy Markdown").clicked() {
                            ui.ctx().copy_text(sql::markdown(result));
                        }
                        if ui.button("Export CSV").clicked() {
                            export = Some(result.clone());
                        }
                        if result.truncated
                            && crate::services::database::is_read_only_query(&result.sql)
                            && ui
                                .add_enabled(
                                    !self.busy.blocks(Lane::Sql(id)),
                                    egui::Button::new("Export all rows..."),
                                )
                                .on_hover_text(
                                    "Runs the query again and streams every row to a CSV file",
                                )
                                .clicked()
                        {
                            export_all = Some(result.sql.clone());
                        }
                        if result.truncated {
                            ui.label(
                                RichText::new("Display limit reached or result incomplete")
                                    .color(theme::gold()),
                            );
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} of {} statements · {} ms",
                                output.completed, output.statements, output.elapsed_ms
                            ))
                            .small()
                            .color(theme::muted()),
                        );
                    });
                });
            });
        if output.error.is_some() || !output.messages.is_empty() {
            egui::Panel::top("result_messages")
                .frame(egui::Frame::new().fill(theme::background()).inner_margin(8))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("result_messages_scroll")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            if let Some(error) = &output.error {
                                let location = output.error_offset.map(|offset| {
                                    let sql = &self.documents[self.document_index].sql;
                                    let base =
                                        self.query_base.get(&id).copied().unwrap_or_default();
                                    (base + offset, line_and_column(sql, base + offset))
                                });
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        RichText::new(error)
                                            .color(egui::Color32::from_rgb(223, 113, 111)),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    if let Some((offset, (line, column))) = location {
                                        ui.label(
                                            RichText::new(format!("Line {line}, column {column}"))
                                                .small()
                                                .color(theme::muted()),
                                        );
                                        if ui.small_button("Show in editor").clicked() {
                                            show_error = Some(offset);
                                        }
                                    }
                                    if ui.small_button("Copy error").clicked() {
                                        ui.ctx().copy_text(error.clone());
                                    }
                                });
                            }
                            for message in &output.messages {
                                ui.label(RichText::new(message).small().color(theme::gold()));
                            }
                        });
                });
        }
        if let Some(plan) = self.plans.get(&id).cloned() {
            egui::Frame::new()
                .inner_margin(10)
                .show(ui, |ui| self.plan_view(ui, &plan));
        } else if let Some(result) = output.results.get(self.result_index) {
            if result.columns.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(format!(
                        "Statement completed. {} rows affected.",
                        result.affected
                    ));
                });
            } else {
                let key = (id, self.result_index);
                if self.results_key != Some(key) {
                    self.results_key = Some(key);
                    self.results_grid.clear();
                }
                let action = grid::show(
                    ui,
                    ("results", id, self.result_index),
                    result,
                    &mut self.results_grid,
                    grid::GridOptions {
                        empty_message: "This statement returned no columns.",
                        ..Default::default()
                    },
                    &mut |_, _, _| {},
                );
                if let Some((row, column)) = action.edit {
                    inspect = Some((
                        result.columns[column].clone(),
                        result.rows[row][column].clone(),
                        column,
                        row,
                    ));
                }
                let mut selected: Vec<usize> = self.results_grid.selection.indices().collect();
                if selected.is_empty()
                    && let Some(row) = self.results_grid.current_row()
                {
                    selected.push(row);
                }
                let rows: Vec<&Row> = selected
                    .iter()
                    .filter_map(|r| result.rows.get(*r))
                    .collect();
                let text = match action.copy {
                    Some(CopyFormat::Tsv) => Some(sql::tsv_rows(None, &rows)),
                    Some(CopyFormat::Csv) => Some(sql::csv_rows(&result.columns, &rows)),
                    Some(CopyFormat::Json) => Some(sql::json_rows(&result.columns, &rows)),
                    Some(CopyFormat::Markdown) => Some(sql::markdown_rows(&result.columns, &rows)),
                    Some(CopyFormat::Insert) => Some(sql::insert_statements(
                        &Relation {
                            schema: "public".into(),
                            name: "query_result".into(),
                            ..Default::default()
                        },
                        &result.columns,
                        &rows
                            .iter()
                            .map(|row| row.iter().map(Option::as_deref).collect())
                            .collect::<Vec<_>>(),
                    )),
                    None => action.copy_column.map(|column| {
                        let values: Vec<Row> = (0..result.row_count())
                            .map(|row| vec![result.rows[row][column].clone()])
                            .collect();
                        sql::tsv_rows(None, &values.iter().collect::<Vec<_>>())
                    }),
                };
                if let Some(text) = text {
                    ui.ctx().copy_text(text);
                    copied = true;
                }
            }
        } else if output.error.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Nothing to execute.").color(theme::muted()));
            });
        }
        if let Some((name, value, column, row)) = inspect {
            self.dialog = Some(Dialog::Cell {
                row,
                column,
                name,
                value: value.clone().unwrap_or_default(),
                null: value.is_none(),
                editable: false,
                pretty: false,
            });
        }
        if copied {
            self.status = "Copied to the clipboard".into();
        }
        if let Some(sql) = export_all {
            self.export_query(sql);
        }
        if let Some(offset) = show_error {
            self.pending_cursor = Some((id, offset));
        }
        if let Some(result) = export {
            self.export_result(&result);
        }
    }

    fn export_query(&mut self, sql: String) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("results.csv")
            .save_file()
        {
            let document = self.documents[self.document_index].id;
            self.send(
                Command::ExportQuery(document, sql, path),
                "Exporting rows...",
            );
        }
    }

    pub(super) fn export_result(&mut self, result: &QueryResult) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("results.csv")
            .save_file()
        {
            match storage::write_user_file(&path, sql::csv(result).as_bytes()) {
                Ok(()) => self.status = format!("Exported {} displayed rows", result.rows.len()),
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }
}
