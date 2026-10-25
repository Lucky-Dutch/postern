use super::*;
use crate::ui::display;

/// What a dialog asked for once it closes.
enum DialogOutcome {
    None,
    Stage(usize, usize, Option<String>),
    RemoveDocument(Uuid),
    Import,
    Quit,
    /// Statements to run, whether they share a transaction, a description, the
    /// relation to reopen, and whether the database list changes.
    Run(Vec<String>, bool, String, Option<(String, String)>, bool),
}

fn danger(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).color(egui::Color32::WHITE))
            .fill(egui::Color32::from_rgb(153, 56, 59)),
    )
    .clicked()
}

fn preview(ui: &mut egui::Ui, sql: &str) {
    egui::Frame::new()
        .fill(theme::toolbar())
        .inner_margin(8)
        .corner_radius(4)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(sql).monospace()).wrap());
        });
}

impl PosternApp {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(error) = self.error.clone() {
            egui::Modal::new(egui::Id::unique("error")).show(ctx, |ui| {
                ui.set_width(540.0);
                ui.heading("Operation could not be completed");
                ui.add_space(12.0);
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(&error).selectable(true));
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Copy error").clicked() {
                        ctx.copy_text(error.clone());
                    }
                    if theme::primary(ui, "OK").clicked() {
                        self.error = None;
                    }
                });
            });
            return;
        }
        if let Some(mut dialog) = self.dialog.take() {
            let mut close = false;
            let mut outcome = DialogOutcome::None;
            let modal = egui::Modal::new(egui::Id::unique("edit_dialog")).show(ctx, |ui| {
                ui.set_width(600.0);
                match &mut dialog {
                    Dialog::Cell {
                        row,
                        column,
                        name,
                        value,
                        null,
                        editable,
                        pretty,
                    } => {
                        ui.heading(if *editable {
                            format!("Edit {name}")
                        } else {
                            format!("Inspect {name}")
                        });
                        ui.add_space(10.0);
                        let json = display::pretty_json(value);
                        ui.horizontal(|ui| {
                            ui.add_enabled(*editable, egui::Checkbox::new(null, "NULL"));
                            if let Some(formatted) = &json {
                                if *editable {
                                    if ui
                                        .add_enabled(!*null, egui::Button::new("Format JSON"))
                                        .clicked()
                                    {
                                        *value = formatted.clone();
                                    }
                                } else {
                                    ui.checkbox(pretty, "Formatted JSON");
                                }
                            }
                            ui.label(
                                RichText::new(format!("{} characters", value.chars().count()))
                                    .small()
                                    .color(theme::muted()),
                            );
                        });
                        egui::ScrollArea::both().max_height(380.0).show(ui, |ui| {
                            if *editable {
                                ui.add_enabled(
                                    !*null,
                                    egui::TextEdit::multiline(value)
                                        .code_editor()
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(14),
                                );
                            } else {
                                let mut shown = match (&json, *pretty) {
                                    (Some(formatted), true) => formatted.clone(),
                                    _ => value.clone(),
                                };
                                ui.add(
                                    egui::TextEdit::multiline(&mut shown)
                                        .code_editor()
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(14),
                                );
                            }
                        });
                        if !*editable {
                            ui.label(
                                RichText::new(
                                    "Read-only. Rows of base tables with a primary key can be edited in Content.",
                                )
                                .small()
                                .color(theme::muted()),
                            );
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Copy").clicked() {
                                ctx.copy_text(if *null { "NULL".into() } else { value.clone() });
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                            if *editable
                                && theme::primary(ui, "Apply")
                                    .on_hover_text("Stage this change. Save changes to write it.")
                                    .clicked()
                            {
                                outcome = DialogOutcome::Stage(
                                    *row,
                                    *column,
                                    (!*null).then(|| value.clone()),
                                );
                                close = true;
                            }
                        });
                    }
                    Dialog::ImportUrl(url) => {
                        ui.heading("New server from URL");
                        ui.label("Paste a postgresql:// connection URL or a libpq connection string.");
                        ui.add(
                            egui::TextEdit::singleline(url)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if theme::primary(ui, "Import").clicked() {
                                match ConnectionProfile::from_connection_string(url) {
                                    Ok((profile, password)) => {
                                        self.draft = profile;
                                        self.password = password;
                                        self.discovered_databases = None;
                                        self.remember_password = false;
                                        self.servers_visible = true;
                                        close = true;
                                    }
                                    Err(error) => self.error = Some(error.to_string()),
                                }
                            }
                        });
                    }
                    Dialog::RemoveDocument(id) => {
                        let document = self.documents.iter().find(|d| d.id == *id);
                        let title = document.map_or("Query", |d| d.title.as_str());
                        ui.heading(format!("Remove query \"{title}\"?"));
                        ui.label("The SQL text is removed from Postern. The file on disk is not deleted.");
                        if let Some(path) = document.and_then(|d| d.path.as_ref()) {
                            ui.monospace(path.display().to_string());
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new("Remove query")
                                        .fill(egui::Color32::from_rgb(153, 56, 59)),
                                )
                                .clicked()
                            {
                                outcome = DialogOutcome::RemoveDocument(*id);
                                close = true;
                            }
                        });
                    }
                    Dialog::ImportCsv(draft) => {
                        ui.heading("Import CSV");
                        ui.monospace(draft.path.display().to_string());
                        ui.add_space(8.0);
                        let mut remap = false;
                        ui.horizontal(|ui| {
                            remap |= ui
                                .checkbox(&mut draft.header, "First line is a header")
                                .changed();
                            ui.label("Delimiter");
                            for (label, delimiter) in
                                [("Comma", ','), ("Semicolon", ';'), ("Tab", '\t'), ("Pipe", '|')]
                            {
                                remap |= ui
                                    .selectable_value(&mut draft.delimiter, delimiter, label)
                                    .changed();
                            }
                        });
                        if remap
                            && let Ok(line) = std::fs::read_to_string(&draft.path)
                                .map(|text| text.lines().next().unwrap_or_default().to_owned())
                        {
                            draft.fields = sql::parse_csv_record(
                                line.trim_start_matches('\u{feff}'),
                                draft.delimiter,
                            );
                            self.map_csv_fields(draft);
                        }
                        ui.label(
                            RichText::new(
                                "Each field goes to the chosen column. An unquoted empty field is NULL; a quoted empty field is an empty string. The import runs in one transaction.",
                            )
                            .small()
                            .color(theme::muted()),
                        );
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            egui::Grid::new("csv_mapping").striped(true).show(ui, |ui| {
                                ui.strong(if draft.header { "File field" } else { "Position" });
                                ui.strong("Table column");
                                ui.end_row();
                                for (index, field) in draft.fields.iter().enumerate() {
                                    if draft.header {
                                        ui.label(field);
                                    } else {
                                        ui.label(format!("{} ({field})", index + 1));
                                    }
                                    let selected = &mut draft.mapping[index];
                                    egui::ComboBox::from_id_salt(("csv_column", index))
                                        .selected_text(if selected.is_empty() {
                                            "Choose..."
                                        } else {
                                            selected.as_str()
                                        })
                                        .show_ui(ui, |ui| {
                                            for column in
                                                self.structure.columns.iter().filter(|c| !c.generated)
                                            {
                                                ui.selectable_value(
                                                    selected,
                                                    column.name.clone(),
                                                    &column.name,
                                                );
                                            }
                                        });
                                    ui.end_row();
                                }
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui
                                .add_enabled(
                                    !self.busy.blocks(Lane::Data),
                                    egui::Button::new(
                                        RichText::new("Import").color(egui::Color32::WHITE),
                                    )
                                    .fill(theme::accent()),
                                )
                                .clicked()
                            {
                                outcome = DialogOutcome::Import;
                            }
                        });
                    }
                    Dialog::RelationAction { relation, action } => {
                        let statement = sql::relation_action_sql(relation, action);
                        let title = match action {
                            RelationAction::Truncate { .. } => "Truncate",
                            RelationAction::Drop { .. } => "Drop",
                            RelationAction::Rename(_) => "Rename",
                            _ => "Run",
                        };
                        ui.heading(format!("{title} {} {}?", relation.kind_label(), relation.name));
                        ui.add_space(8.0);
                        match action {
                            RelationAction::Truncate { cascade, restart_identity } => {
                                ui.label("Every row is removed. This cannot be undone.");
                                ui.checkbox(restart_identity, "Restart identity sequences");
                                ui.checkbox(cascade, "Also truncate tables with foreign keys to this table (CASCADE)");
                            }
                            RelationAction::Drop { cascade } => {
                                ui.label("The object and its data are removed. This cannot be undone.");
                                ui.checkbox(cascade, "Also drop dependent objects (CASCADE)");
                            }
                            RelationAction::Rename(name) => {
                                ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                            }
                            _ => {}
                        }
                        ui.add_space(8.0);
                        match &statement {
                            Ok(sql) => preview(ui, sql),
                            Err(error) => {
                                ui.label(RichText::new(error.to_string()).color(theme::gold()));
                            }
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            let destructive = !matches!(action, RelationAction::Rename(_));
                            let ready = statement.is_ok() && !self.busy.blocks(Lane::Data);
                            let clicked = if destructive {
                                danger(ui, title, ready)
                            } else {
                                ui.add_enabled(ready, egui::Button::new(title)).clicked()
                            };
                            if clicked && let Ok(sql) = statement {
                                let reopen = match action {
                                    RelationAction::Drop { .. } => None,
                                    RelationAction::Rename(name) => Some((relation.schema.clone(), name.trim().to_owned())),
                                    _ => Some((relation.schema.clone(), relation.name.clone())),
                                };
                                outcome = DialogOutcome::Run(vec![sql.clone()], true, format!("{sql} completed"), reopen, false);
                                close = true;
                            }
                        });
                    }
                    Dialog::DropRoutine { routine, cascade } => {
                        ui.heading(format!("Drop {} {}?", if routine.procedure { "procedure" } else { "function" }, routine.name));
                        ui.checkbox(cascade, "Also drop dependent objects (CASCADE)");
                        let statement = sql::drop_routine_sql(routine, *cascade);
                        preview(ui, &statement);
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if danger(ui, "Drop", !self.busy.blocks(Lane::Data)) {
                                outcome = DialogOutcome::Run(vec![statement.clone()], true, format!("Dropped {}", routine.name), None, false);
                                close = true;
                            }
                        });
                    }
                    Dialog::CreateSchema(name) => {
                        ui.heading("New schema");
                        ui.add(egui::TextEdit::singleline(name).hint_text("Schema name").desired_width(f32::INFINITY));
                        let statement = sql::create_schema_sql(name);
                        if let Ok(sql) = &statement {
                            preview(ui, sql);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui.add_enabled(statement.is_ok() && !self.busy.blocks(Lane::Data), egui::Button::new("Create")).clicked()
                                && let Ok(sql) = statement
                            {
                                outcome = DialogOutcome::Run(vec![sql], true, format!("Created schema {}", name.trim()), None, false);
                                close = true;
                            }
                        });
                    }
                    Dialog::KeyPicker {
                        row,
                        column,
                        key,
                        search,
                        requested,
                        results,
                    } => {
                        let target = key.referenced_columns.first().cloned().unwrap_or_default();
                        ui.heading(format!(
                            "Choose {}.{} ({target})",
                            key.referenced_schema, key.referenced_table
                        ));
                        ui.add(
                            egui::TextEdit::singleline(search)
                                .hint_text("Search by key or any value in the row")
                                .desired_width(f32::INFINITY),
                        )
                        .request_focus();
                        if requested.as_deref() != Some(search.as_str())
                            && !self.busy.blocks(Lane::Data)
                            && self.send(
                                Command::LookupKeys(
                                    Relation {
                                        schema: key.referenced_schema.clone(),
                                        name: key.referenced_table.clone(),
                                        kind: "r".into(),
                                        ..Default::default()
                                    },
                                    target.clone(),
                                    search.clone(),
                                ),
                                "Searching...",
                            )
                        {
                            *requested = Some(search.clone());
                        }
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                            if results.is_empty() {
                                ui.label(RichText::new("No matching rows.").color(theme::muted()));
                            }
                            for (value, preview) in results.iter() {
                                ui.horizontal(|ui| {
                                    if ui.button(RichText::new(value).monospace()).clicked() {
                                        outcome = DialogOutcome::Stage(*row, *column, Some(value.clone()));
                                        close = true;
                                    }
                                    ui.add(
                                        egui::Label::new(RichText::new(preview).small().color(theme::muted()))
                                            .truncate(),
                                    );
                                });
                            }
                        });
                        ui.add_space(8.0);
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                    Dialog::MoveToFolder { document, folder } => {
                        ui.heading("Move to folder");
                        ui.add(
                            egui::TextEdit::singleline(folder)
                                .hint_text("Folder name; empty removes the folder")
                                .desired_width(f32::INFINITY),
                        );
                        let mut existing: Vec<String> = self
                            .documents
                            .iter()
                            .map(|d| d.folder.trim().to_owned())
                            .filter(|f| !f.is_empty())
                            .collect();
                        existing.sort();
                        existing.dedup();
                        ui.horizontal_wrapped(|ui| {
                            for name in existing {
                                if ui.small_button(&name).clicked() {
                                    *folder = name;
                                }
                            }
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if theme::primary(ui, "Move").clicked() {
                                if let Some(target) = self.documents.iter_mut().find(|d| d.id == *document) {
                                    target.folder = folder.trim().to_owned();
                                }
                                self.save_drafts();
                                close = true;
                            }
                        });
                    }
                    Dialog::CreateDatabase(name) => {
                        ui.heading("New database");
                        ui.add(egui::TextEdit::singleline(name).hint_text("Database name").desired_width(f32::INFINITY));
                        let statement = sql::create_database_sql(name);
                        if let Ok(sql) = &statement {
                            preview(ui, sql);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui.add_enabled(statement.is_ok() && !self.busy.blocks(Lane::Data), egui::Button::new("Create")).clicked()
                                && let Ok(sql) = statement
                            {
                                outcome = DialogOutcome::Run(vec![sql], false, format!("Created database {}", name.trim()), None, true);
                                close = true;
                            }
                        });
                    }
                    Dialog::DropDatabase { name, confirmation } => {
                        ui.heading(format!("Drop database {name}?"));
                        ui.label("Every table and all data in this database are removed. This cannot be undone. Other sessions must be disconnected first.");
                        ui.label(format!("Type {name} to confirm:"));
                        ui.add(egui::TextEdit::singleline(confirmation).desired_width(f32::INFINITY));
                        let statement = sql::drop_database_sql(name);
                        if let Ok(sql) = &statement {
                            preview(ui, sql);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if danger(ui, "Drop database", confirmation.trim() == name.as_str() && statement.is_ok() && !self.busy.blocks(Lane::Data))
                                && let Ok(sql) = statement
                            {
                                outcome = DialogOutcome::Run(vec![sql], false, format!("Dropped database {name}"), None, true);
                                close = true;
                            }
                        });
                    }
                    Dialog::ReviewDdl { statements, description, reopen } => {
                        ui.heading("Review SQL");
                        ui.label("These statements run in one transaction. If one fails, none are applied.");
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            preview(ui, &statements.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n"));
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Back").clicked() {
                                close = true;
                            }
                            if ui.button("Copy SQL").clicked() {
                                ctx.copy_text(statements.iter().map(|s| format!("{s};\n")).collect());
                            }
                            if ui.add_enabled(!self.busy.blocks(Lane::Data), egui::Button::new(RichText::new("Apply").color(egui::Color32::WHITE)).fill(theme::accent())).clicked() {
                                outcome = DialogOutcome::Run(statements.clone(), true, description.clone(), reopen.clone(), false);
                                close = true;
                            }
                        });
                    }
                    Dialog::Terminate { pid, description } => {
                        ui.heading(format!("Terminate session {pid}?"));
                        ui.label(description.as_str());
                        ui.label("The session's open transaction is rolled back and its client is disconnected.");
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if danger(ui, "Terminate", !self.busy.blocks(Lane::Data)) {
                                self.send(Command::SignalBackend(*pid, true), "Terminating session...");
                                close = true;
                            }
                        });
                    }
                    Dialog::ResetQueryStats => {
                        ui.heading("Reset query statistics?");
                        ui.label("pg_stat_statements_reset() clears the statistics of every database on this server. It requires a superuser or a role granted EXECUTE on the function.");
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if danger(ui, "Reset statistics", !self.busy.blocks(Lane::Data)) {
                                self.send(Command::ResetQueryStats(self.slow_request), "Resetting query statistics...");
                                close = true;
                            }
                        });
                    }
                    Dialog::SetUpQueryStats { data_directory, log_file, other_sessions, configure } => {
                        let database = self.connected.as_ref().map(|p| p.database.clone()).unwrap_or_default();
                        ui.heading("Restart the server to load pg_stat_statements?");
                        ui.add_space(4.0);
                        ui.label("Postern will:");
                        let mut steps = Vec::new();
                        if *configure {
                            steps.push("Add pg_stat_statements to shared_preload_libraries with ALTER SYSTEM, keeping the libraries already configured.".to_owned());
                        }
                        steps.push(format!("Restart the server at {data_directory} with a fast shutdown and its current options."));
                        steps.push(format!("Reconnect and create the extension in {database}."));
                        for (index, step) in steps.iter().enumerate() {
                            ui.label(format!("{}. {step}", index + 1));
                        }
                        ui.add_space(6.0);
                        let others = match *other_sessions {
                            0 => "No other application is connected.".to_owned(),
                            1 => "1 session of another application will be disconnected, and its open transaction rolled back.".to_owned(),
                            n => format!("{n} sessions of other applications will be disconnected, and their open transactions rolled back."),
                        };
                        ui.label(RichText::new(others).color(if *other_sessions > 0 { theme::gold() } else { theme::muted() }));
                        ui.label(RichText::new(format!(
                            "Server output is appended to {log_file}. If the server does not start, Postern starts it again with its previous settings."
                        )).small().color(theme::muted()));
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if danger(ui, "Restart server", !self.busy.any()) {
                                self.send(Command::SetUpQueryStats(self.slow_request), "Setting up pg_stat_statements...");
                                close = true;
                            }
                        });
                    }
                    Dialog::QuitWithChanges => {
                        ui.heading("Discard unsaved changes?");
                        ui.label(format!(
                            "{} changes in the table browser have not been saved.",
                            self.edits.len()
                        ));
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new("Discard and quit")
                                        .fill(egui::Color32::from_rgb(153, 56, 59)),
                                )
                                .clicked()
                            {
                                outcome = DialogOutcome::Quit;
                                close = true;
                            }
                        });
                    }
                }
            });
            if modal.should_close() {
                close = true;
            }
            match outcome {
                DialogOutcome::None => {}
                DialogOutcome::Stage(row, column, value) => {
                    self.edits
                        .set(&self.table_page.result.rows, row, column, value);
                }
                DialogOutcome::RemoveDocument(id) => self.remove_document(id),
                DialogOutcome::Import => {
                    if let Dialog::ImportCsv(draft) = &dialog {
                        match self.start_csv_import(draft) {
                            Ok(()) => close = true,
                            Err(error) => self.warning = Some(error.to_string()),
                        }
                    }
                }
                DialogOutcome::Quit => {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                DialogOutcome::Run(statements, transactional, description, reopen, databases) => {
                    self.refresh_databases_after_ddl = databases;
                    self.run_ddl(statements, transactional, &description, reopen);
                }
            }
            if !close {
                self.dialog = Some(dialog);
            }
        }
        if self.show_history {
            let mut open = true;
            let mut selected = None;
            egui::Window::new("Query History")
                .open(&mut open)
                .default_width(650.0)
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("Last 100 user queries, saved locally.")
                            .small()
                            .color(theme::muted()),
                    );
                    if ui.button("Clear history").clicked() {
                        self.history.clear();
                        if self.history_writable
                            && let Some(store) = &self.store
                            && let Err(e) = store.save_history(&self.history)
                        {
                            self.error = Some(e.to_string());
                        }
                    }
                    egui::ScrollArea::vertical()
                        .max_height(450.0)
                        .show(ui, |ui| {
                            for query in &self.history {
                                ui.group(|ui| {
                                    ui.monospace(query.chars().take(500).collect::<String>());
                                    if ui
                                        .add_enabled(
                                            !self.busy.control,
                                            egui::Button::new("Open in new query"),
                                        )
                                        .clicked()
                                    {
                                        selected = Some(query.clone());
                                    }
                                });
                            }
                        });
                });
            self.show_history = open;
            if let Some(query) = selected {
                self.new_document();
                self.documents[self.document_index].sql = query;
                self.save_drafts();
                self.show_history = false;
            }
        }
    }
}
