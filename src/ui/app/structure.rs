//! Table structure: the read-only overview with statistics, and the structure
//! editor that generates reviewed ALTER TABLE or CREATE TABLE statements.

use super::*;

const COMMON_TYPES: &[&str] = &[
    "text",
    "varchar(255)",
    "integer",
    "bigint",
    "numeric(12,2)",
    "boolean",
    "date",
    "timestamptz",
    "timestamp",
    "uuid",
    "jsonb",
    "bytea",
    "text[]",
];

impl PosternApp {
    pub(super) fn structure_view(&mut self, ui: &mut egui::Ui) {
        if self.table_editor.is_some() {
            self.table_editor_ui(ui);
            return;
        }
        let editable = self
            .selected_relation
            .as_ref()
            .is_some_and(Relation::is_table)
            && !self.structure.columns.is_empty();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Frame::new().inner_margin(18).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(comment) = &self.structure.comment {
                        ui.label(RichText::new(comment).italics().color(theme::muted()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                editable && !self.busy.blocks(Lane::Data),
                                egui::Button::new("Edit structure"),
                            )
                            .on_hover_text("Change columns, constraints, and indexes. Changes are reviewed as SQL before they run.")
                            .clicked()
                            && !self.blocked_by_edits()
                            && let Some(relation) = &self.selected_relation
                        {
                            let draft = TableDraft::from_structure(relation, &self.structure);
                            self.table_editor = Some(TableEditor {
                                original: draft.clone(),
                                edited: draft,
                                creating: false,
                            });
                        }
                    });
                });
                if let Some(stats) = &self.structure.stats {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let mut fact = |label: &str, value: String| {
                            ui.label(RichText::new(label).small().color(theme::muted()));
                            ui.label(RichText::new(value).small());
                            ui.add_space(10.0);
                        };
                        fact("Total size", bytes(stats.total_bytes));
                        fact("Table", bytes(stats.table_bytes));
                        fact("Indexes", bytes(stats.index_bytes));
                        if let Some(rows) = stats.live_rows {
                            fact("Live rows", rows.to_string());
                        }
                        if let Some(rows) = stats.dead_rows {
                            fact("Dead rows", rows.to_string());
                        }
                        if let Some(scans) = stats.sequential_scans {
                            fact("Sequential scans", scans.to_string());
                        }
                        if let Some(scans) = stats.index_scans {
                            fact("Index scans", scans.to_string());
                        }
                        fact(
                            "Last vacuum",
                            stats.last_vacuum.clone().unwrap_or_else(|| "never".into()),
                        );
                        fact(
                            "Last analyze",
                            stats.last_analyze.clone().unwrap_or_else(|| "never".into()),
                        );
                    });
                }
                ui.add_space(10.0);
                ui.heading("Columns");
                ui.add_space(10.0);
                egui::Grid::new("columns")
                    .striped(true)
                    .min_col_width(80.0)
                    .spacing([24.0, 12.0])
                    .show(ui, |ui| {
                        for title in ["Name", "Type", "Nullable", "Default", "Key", "Comment"] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for column in &self.structure.columns {
                            ui.label(&column.name);
                            ui.monospace(&column.data_type);
                            ui.label(if column.nullable { "Yes" } else { "No" });
                            ui.label(column.default.as_deref().unwrap_or(if column.generated {
                                "Generated"
                            } else {
                                ""
                            }));
                            let foreign = self
                                .structure
                                .foreign_keys
                                .iter()
                                .any(|key| key.columns.contains(&column.name));
                            ui.label(
                                RichText::new(match (column.primary_key, foreign) {
                                    (true, true) => "PRIMARY, FOREIGN",
                                    (true, false) => "PRIMARY",
                                    (false, true) => "FOREIGN",
                                    (false, false) => "",
                                })
                                .color(theme::gold()),
                            );
                            ui.label(
                                RichText::new(column.comment.as_deref().unwrap_or_default())
                                    .color(theme::muted()),
                            );
                            ui.end_row();
                        }
                    });
                ui.add_space(25.0);
                ui.heading("Constraints");
                ui.add_space(10.0);
                if self.structure.constraints.is_empty() {
                    ui.label(RichText::new("None").color(theme::muted()));
                }
                for (name, definition) in &self.structure.constraints {
                    ui.strong(name);
                    ui.monospace(definition);
                    ui.add_space(7.0);
                }
                ui.add_space(25.0);
                ui.heading("Indexes");
                ui.add_space(10.0);
                if self.structure.indexes.is_empty() {
                    ui.label(RichText::new("None").color(theme::muted()));
                }
                for index in &self.structure.indexes {
                    ui.horizontal(|ui| {
                        ui.strong(&index.name);
                        let mut facts = Vec::new();
                        if let Some(size) = index.bytes {
                            facts.push(bytes(size));
                        }
                        if let Some(scans) = index.scans {
                            facts.push(format!("{scans} scans"));
                        }
                        if index.constraint {
                            facts.push("constraint".into());
                        }
                        ui.label(RichText::new(facts.join(" · ")).small().color(theme::muted()));
                    });
                    ui.monospace(&index.definition);
                    ui.add_space(7.0);
                }
                ui.add_space(25.0);
                ui.heading("Privileges");
                ui.add_space(10.0);
                if self.structure.privileges.is_empty() {
                    ui.label(RichText::new("No grants visible to this login.").color(theme::muted()));
                }
                for (grantee, privileges) in &self.structure.privileges {
                    ui.horizontal(|ui| {
                        ui.strong(grantee);
                        ui.label(RichText::new(privileges).color(theme::muted()));
                    });
                }
                ui.add_space(25.0);
                ui.heading("Referenced by");
                ui.add_space(10.0);
                if self.structure.referenced_by.is_empty() {
                    ui.label(RichText::new("None").color(theme::muted()));
                }
                for key in &self.structure.referenced_by {
                    ui.monospace(format!(
                        "{}.{} ({}) via {}",
                        key.schema,
                        key.table,
                        key.columns.join(", "),
                        key.name
                    ));
                }
            });
        });
    }

    pub(super) fn start_new_table(&mut self, schema: String) {
        if self.blocked_by_edits() {
            return;
        }
        let draft = TableDraft {
            schema,
            name: String::new(),
            columns: vec![ColumnDraft {
                name: "id".into(),
                data_type: "bigint GENERATED ALWAYS AS IDENTITY".into(),
                primary_key: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        self.table_editor = Some(TableEditor {
            original: draft.clone(),
            edited: draft,
            creating: true,
        });
        self.mode = Mode::NewTable;
    }

    pub(super) fn table_editor_view(&mut self, ui: &mut egui::Ui) {
        if self.table_editor.is_none() {
            self.mode = Mode::Query;
            return;
        }
        self.table_editor_ui(ui);
    }

    fn table_editor_ui(&mut self, ui: &mut egui::Ui) {
        let schemas: Vec<String> = {
            let mut schemas: Vec<String> =
                self.relations.iter().map(|r| r.schema.clone()).collect();
            schemas.push("public".into());
            schemas.sort();
            schemas.dedup();
            schemas
        };
        let mut cancel = false;
        let mut review = false;
        let Some(editor) = &mut self.table_editor else {
            return;
        };
        let creating = editor.creating;
        egui::Panel::bottom("structure_editor_actions")
            .exact_size(48.0)
            .frame(egui::Frame::new().fill(theme::toolbar()).inner_margin(9))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if theme::primary(
                        ui,
                        if creating {
                            "Review CREATE TABLE..."
                        } else {
                            "Review changes..."
                        },
                    )
                    .clicked()
                    {
                        review = true;
                    }
                    ui.label(
                        RichText::new("Nothing changes until you review and apply the SQL.")
                            .small()
                            .color(theme::muted()),
                    );
                });
            });
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Frame::new().inner_margin(18).show(ui, |ui| {
                let draft = &mut editor.edited;
                ui.heading(if creating { "New table" } else { "Edit structure" });
                ui.add_space(8.0);
                egui::Grid::new("table_identity").spacing([12.0, 8.0]).show(ui, |ui| {
                    if creating {
                        ui.label("Schema");
                        egui::ComboBox::from_id_salt("new_table_schema")
                            .selected_text(&draft.schema)
                            .show_ui(ui, |ui| {
                                for schema in &schemas {
                                    ui.selectable_value(&mut draft.schema, schema.clone(), schema);
                                }
                            });
                        ui.end_row();
                    }
                    ui.label("Name");
                    field(ui, true, &mut draft.name, 320.0, false, "table name");
                    ui.end_row();
                    ui.label("Comment");
                    field(ui, true, &mut draft.comment, 480.0, false, "optional");
                    ui.end_row();
                });
                ui.add_space(14.0);
                ui.heading("Columns");
                egui::Grid::new("draft_columns")
                    .striped(true)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        for title in ["Name", "Type", "", "Null", "Default", "Comment"] {
                            ui.strong(title);
                        }
                        if creating {
                            ui.strong("Key");
                        }
                        ui.end_row();
                        for (index, column) in draft.columns.iter_mut().enumerate() {
                            let enabled = !column.dropped;
                            ui.push_id(("column", index), |ui| {
                                field(ui, enabled, &mut column.name, 150.0, false, "name");
                            });
                            ui.push_id(("type", index), |ui| {
                                field(ui, enabled, &mut column.data_type, 200.0, true, "type");
                            });
                            ui.push_id(("types", index), |ui| {
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.menu_button("▾", |ui| {
                                        for name in COMMON_TYPES {
                                            if ui.button(*name).clicked() {
                                                column.data_type = (*name).into();
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                            });
                            ui.add_enabled(enabled, egui::Checkbox::without_text(&mut column.nullable));
                            ui.push_id(("default", index), |ui| {
                                let hint = if column.generated { "generated" } else { "none" };
                                field(ui, enabled && !column.generated, &mut column.default, 170.0, true, hint);
                            });
                            ui.push_id(("comment", index), |ui| {
                                field(ui, enabled, &mut column.comment, 200.0, false, "");
                            });
                            if creating {
                                ui.checkbox(&mut column.primary_key, "");
                            }
                            ui.push_id(("drop", index), |ui| {
                                if ui
                                    .small_button(if column.dropped { "Restore" } else { "Drop" })
                                    .clicked()
                                {
                                    column.dropped = !column.dropped;
                                }
                            });
                            ui.end_row();
                        }
                    });
                if ui.small_button("+ Column").clicked() {
                    draft.columns.push(ColumnDraft {
                        data_type: "text".into(),
                        nullable: true,
                        ..Default::default()
                    });
                }
                ui.add_space(14.0);
                ui.heading("Constraints");
                ui.label(
                    RichText::new("For example CHECK (price >= 0), UNIQUE (email), or FOREIGN KEY (author_id) REFERENCES authors (id).")
                        .small()
                        .color(theme::muted()),
                );
                egui::Grid::new("draft_constraints").striped(true).spacing([10.0, 6.0]).show(ui, |ui| {
                    for (index, constraint) in draft.constraints.iter_mut().enumerate() {
                        let enabled = constraint.original.is_none() && !constraint.dropped;
                        ui.push_id(("constraint_name", index), |ui| {
                            field(ui, enabled, &mut constraint.name, 170.0, false, "optional name");
                        });
                        ui.push_id(("constraint_definition", index), |ui| {
                            field(ui, enabled, &mut constraint.definition, 480.0, true, "definition");
                        });
                        ui.push_id(("constraint_drop", index), |ui| {
                            if ui
                                .small_button(if constraint.dropped { "Restore" } else { "Drop" })
                                .clicked()
                            {
                                constraint.dropped = !constraint.dropped;
                            }
                        });
                        ui.end_row();
                    }
                });
                if ui.small_button("+ Constraint").clicked() {
                    draft.constraints.push(ConstraintDraft::default());
                }
                ui.add_space(14.0);
                ui.heading("Indexes");
                egui::Grid::new("draft_indexes").striped(true).spacing([10.0, 6.0]).show(ui, |ui| {
                    for (index, draft_index) in draft.indexes.iter_mut().enumerate() {
                        if draft_index.original.is_some() {
                            ui.label(&draft_index.name);
                            ui.add_enabled_ui(!draft_index.dropped, |ui| {
                                ui.monospace(&draft_index.definition);
                            });
                        } else {
                            ui.push_id(("index_name", index), |ui| {
                                field(ui, !draft_index.dropped, &mut draft_index.name, 170.0, false, "optional name");
                            });
                            ui.push_id(("index_definition", index), |ui| {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut draft_index.unique, "Unique");
                                    egui::ComboBox::from_id_salt("method")
                                        .width(70.0)
                                        .selected_text(if draft_index.method.is_empty() {
                                            "btree"
                                        } else {
                                            draft_index.method.as_str()
                                        })
                                        .show_ui(ui, |ui| {
                                            for method in ["btree", "hash", "gin", "gist", "brin", "spgist"] {
                                                ui.selectable_value(&mut draft_index.method, method.into(), method);
                                            }
                                        });
                                    field(
                                        ui,
                                        !draft_index.dropped,
                                        &mut draft_index.columns,
                                        320.0,
                                        true,
                                        "columns, e.g. email or (lower(email))",
                                    );
                                });
                            });
                        }
                        ui.push_id(("index_drop", index), |ui| {
                            if ui
                                .small_button(if draft_index.dropped { "Restore" } else { "Drop" })
                                .clicked()
                            {
                                draft_index.dropped = !draft_index.dropped;
                            }
                        });
                        ui.end_row();
                    }
                });
                if ui.small_button("+ Index").clicked() {
                    draft.indexes.push(IndexDraft {
                        method: "btree".into(),
                        ..Default::default()
                    });
                }
            });
        });
        if cancel {
            self.table_editor = None;
            if creating {
                self.mode = Mode::Query;
            }
            return;
        }
        if review {
            let editor = self.table_editor.as_ref().expect("editor is open");
            let edited = &editor.edited;
            let result = if editor.creating {
                sql::create_table_sql(edited)
            } else {
                sql::alter_table_sql(&editor.original, edited)
            };
            match result {
                Ok(statements) if statements.is_empty() => {
                    self.warning = Some("There are no structure changes to apply.".into());
                }
                Ok(statements) => {
                    self.dialog = Some(Dialog::ReviewDdl {
                        statements,
                        description: if editor.creating {
                            format!("Created {}.{}", edited.schema, edited.name.trim())
                        } else {
                            format!("Updated {}.{}", edited.schema, edited.name.trim())
                        },
                        reopen: Some((edited.schema.clone(), edited.name.trim().to_owned())),
                    });
                }
                Err(error) => self.warning = Some(error.to_string()),
            }
        }
    }
}

pub(super) fn bytes(value: i64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// A single-line text field with a fixed width, which grids would otherwise shrink.
fn field(
    ui: &mut egui::Ui,
    enabled: bool,
    text: &mut String,
    width: f32,
    monospace: bool,
    hint: &str,
) -> egui::Response {
    let mut edit = egui::TextEdit::singleline(text).hint_text(hint);
    if monospace {
        edit = edit.font(egui::TextStyle::Monospace);
    }
    ui.add_enabled_ui(enabled, |ui| ui.add_sized([width, 24.0], edit))
        .inner
}
