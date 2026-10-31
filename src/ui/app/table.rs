use super::*;
use crate::ui::{
    display,
    grid::{CellValue, ColumnStyle, CopyFormat, GridSource, RowState},
};
use egui_extras::syntax_highlighting::code_view_ui;

/// Loaded rows with staged edits overlaid, as shown by the table browser grid.
struct StagedRows<'a> {
    result: &'a QueryResult,
    edits: &'a PendingEdits,
    columns: &'a [Column],
    /// Loaded rows can be changed: a base table with a primary key.
    editable: bool,
    /// New rows can be added: a base table.
    insertable: bool,
}

impl GridSource for StagedRows<'_> {
    fn columns(&self) -> &[String] {
        &self.result.columns
    }

    fn row_count(&self) -> usize {
        self.result.rows.len() + self.edits.inserts.len()
    }

    fn cell(&self, row: usize, column: usize) -> CellValue<'_> {
        let loaded = self.result.rows.len();
        if row < loaded {
            return match self.edits.value(&self.result.rows, row, column) {
                Some(text) => CellValue::Text(text),
                None => CellValue::Null,
            };
        }
        match self
            .edits
            .inserts
            .get(row - loaded)
            .and_then(|values| values.get(column))
        {
            Some(InsertValue::Text(text)) => CellValue::Text(text),
            Some(InsertValue::Null) => CellValue::Null,
            _ => CellValue::Default,
        }
    }

    fn edited(&self, row: usize, column: usize) -> bool {
        self.edits
            .updates
            .get(&row)
            .is_some_and(|cells| cells.contains_key(&column))
    }

    fn row_state(&self, row: usize) -> RowState {
        if row >= self.result.rows.len() {
            RowState::Inserted
        } else if self.edits.is_deleted(row) {
            RowState::Deleted
        } else {
            RowState::Stored
        }
    }

    fn editable(&self, row: usize, column: usize) -> bool {
        if self.columns.get(column).is_none_or(|c| c.generated) {
            return false;
        }
        if row >= self.result.rows.len() {
            self.insertable
        } else {
            self.editable
        }
    }
}

/// Something a cell's context menu asked the table browser to do.
enum CellRequest {
    Filter(Filter),
    Open(Relation, FilterSet),
    PickKey(usize, usize, ForeignKey),
}

impl PosternApp {
    pub(super) fn table_workspace(&mut self, ui: &mut egui::Ui) {
        let data_busy =
            self.busy.blocks(Lane::Data) || self.table_tabs[self.table_index].loading_rows;
        let pending = !self.table_tabs[self.table_index].edits.is_empty();
        egui::Panel::bottom("table_actions")
            .exact_size(44.0)
            .frame(egui::Frame::new().fill(theme::toolbar()).inner_margin(7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.mode, Mode::Content, "Content");
                    ui.selectable_value(&mut self.mode, Mode::Structure, "Structure");
                    ui.selectable_value(&mut self.mode, Mode::Definition, "DDL");
                    ui.separator();
                    if self.mode == Mode::Content {
                        self.edit_actions(ui, data_busy);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.mode == Mode::Content {
                            self.pagination(ui, data_busy || pending);
                        }
                    });
                });
            });
        egui::Panel::top("table_options")
            .frame(
                egui::Frame::new()
                    .fill(theme::background())
                    .inner_margin(10),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !self.table_tabs[self.table_index].back_stack.is_empty()
                        && ui
                            .add_enabled(!data_busy, egui::Button::new("‹ Back"))
                            .on_hover_text("Return to the previous table")
                            .clicked()
                    {
                        self.go_back();
                    }
                    if let Some(relation) = &self.table_tabs[self.table_index].selected_relation {
                        ui.label(
                            RichText::new(format!("{} / {}", relation.schema, relation.name))
                                .color(theme::muted()),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.table_options(ui, data_busy, pending);
                    });
                });
                if self.table_tabs[self.table_index].show_filter && self.mode == Mode::Content {
                    self.filter_bar(ui);
                }
            });
        self.table_tabs[self.table_index].mode = self.mode;
        match self.mode {
            Mode::Content => self.content_view(ui, data_busy),
            Mode::Structure => self.structure_view(ui),
            Mode::Definition => {
                egui::ScrollArea::both().show(ui, |ui| {
                    egui::Frame::new().inner_margin(16).show(ui, |ui| {
                        code_view_ui(
                            ui,
                            &theme::code_theme(14.0),
                            &self.table_tabs[self.table_index].structure.definition,
                            "sql",
                        );
                    });
                });
            }
            Mode::Query
            | Mode::Routine
            | Mode::NewTable
            | Mode::Activity
            | Mode::Roles
            | Mode::Diagnostics => {}
        }
    }

    fn edit_actions(&mut self, ui: &mut egui::Ui, data_busy: bool) {
        let insertable = !data_busy
            && !self.table_tabs[self.table_index]
                .structure
                .columns
                .is_empty()
            && self.table_tabs[self.table_index]
                .selected_relation
                .as_ref()
                .is_some_and(Relation::is_table);
        if ui
            .add_enabled(insertable, egui::Button::new("+ Row"))
            .on_hover_text("Add a new row. It is inserted when you save.")
            .clicked()
        {
            self.stage_insert();
        }
        if ui
            .add_enabled(
                insertable
                    && !self.table_tabs[self.table_index]
                        .content
                        .selection
                        .is_empty(),
                egui::Button::new(format!(
                    "Delete ({})",
                    self.table_tabs[self.table_index].content.selection.len()
                )),
            )
            .on_hover_text("Mark the selected rows for deletion. They are deleted when you save.")
            .clicked()
        {
            self.stage_delete();
        }
        if !self.table_tabs[self.table_index].edits.is_empty() {
            ui.separator();
            let count = self.table_tabs[self.table_index].edits.len();
            if theme::primary(
                ui,
                &format!(
                    "Save {count} {}",
                    if count == 1 { "change" } else { "changes" }
                ),
            )
            .on_hover_text("Save all changes in one transaction (Command-S)")
            .clicked()
            {
                self.save_changes();
            }
            if ui.button("Discard").clicked() {
                self.discard_changes();
            }
        }
    }

    fn pagination(&mut self, ui: &mut egui::Ui, locked: bool) {
        if ui
            .add_enabled(
                !locked && self.table_tabs[self.table_index].table_page.has_more,
                egui::Button::new("Next >"),
            )
            .clicked()
        {
            self.browse(self.table_tabs[self.table_index].page + 1);
        }
        ui.label(format!(
            "Page {}",
            self.table_tabs[self.table_index].page + 1
        ));
        if ui
            .add_enabled(
                !locked && self.table_tabs[self.table_index].page > 0,
                egui::Button::new("< Previous"),
            )
            .clicked()
        {
            self.browse(self.table_tabs[self.table_index].page - 1);
        }
        let counting = self.table_tabs[self.table_index].count_wanted
            || (self.count_tab == Some(self.table_tabs[self.table_index].id)
                && self.busy.sql.contains(&COUNT_SESSION));
        let estimate = self.table_tabs[self.table_index]
            .selected_relation
            .as_ref()
            .and_then(|selected| {
                self.relations
                    .iter()
                    .find(|r| r.schema == selected.schema && r.name == selected.name)
            })
            .map_or(0, |r| r.estimated_rows);
        let shown = self.table_tabs[self.table_index]
            .table_page
            .result
            .rows
            .len();
        let first =
            self.table_tabs[self.table_index].page * self.table_tabs[self.table_index].page_size;
        let range = if shown == 0 {
            "No rows".to_owned()
        } else {
            format!(
                "Rows {}–{}",
                group(first as i64 + 1),
                group((first + shown) as i64)
            )
        };
        let total = match self.table_tabs[self.table_index].row_count {
            Some(count) => format!(" of {}", group(count)),
            None if self.table_tabs[self.table_index].applied_filter.is_empty() && estimate > 0 => {
                format!(" of ≈{}", group(estimate))
            }
            None => String::new(),
        };
        let selected = match self.table_tabs[self.table_index].content.selection.len() {
            0 => String::new(),
            count => format!(", {count} selected"),
        };
        ui.label(RichText::new(format!("{range}{total}{selected}")).color(theme::muted()))
            .on_hover_text(if self.table_tabs[self.table_index].row_count.is_some() {
                "Exact number of rows matching the filter"
            } else {
                "≈ is PostgreSQL's estimate from the last ANALYZE"
            });
        if counting {
            ui.spinner().on_hover_text("Counting rows");
        } else if self.table_tabs[self.table_index].row_count.is_none()
            && ui
                .add_enabled(
                    !self.table_tabs[self.table_index]
                        .structure
                        .columns
                        .is_empty(),
                    egui::Button::new("Count").small(),
                )
                .on_hover_text("Count all rows matching the filter")
                .clicked()
        {
            self.request_count(true);
        }
    }

    fn table_options(&mut self, ui: &mut egui::Ui, data_busy: bool, pending: bool) {
        if ui
            .add_enabled(!data_busy && !pending, egui::Button::new("Refresh"))
            .on_hover_text("Reload rows and keep the filter, sort, and page (Command-R)")
            .clicked()
        {
            self.refresh();
        }
        if self.mode != Mode::Content {
            return;
        }
        let filtered = !self.table_tabs[self.table_index].applied_filter.is_empty();
        if ui
            .button(if filtered { "Filter •" } else { "Filter" })
            .clicked()
        {
            self.table_tabs[self.table_index].show_filter =
                !self.table_tabs[self.table_index].show_filter;
            if self.table_tabs[self.table_index].show_filter
                && self.table_tabs[self.table_index]
                    .filter
                    .conditions
                    .is_empty()
            {
                self.table_tabs[self.table_index].filter =
                    self.table_tabs[self.table_index].applied_filter.clone();
                if self.table_tabs[self.table_index]
                    .filter
                    .conditions
                    .is_empty()
                {
                    self.add_filter_condition();
                }
            }
        }
        ui.toggle_value(
            &mut self.table_tabs[self.table_index].show_details,
            "Details",
        )
        .on_hover_text("Show the selected row's values in a side panel");
        let old = (
            self.table_tabs[self.table_index].page_size,
            self.table_tabs[self.table_index].nulls,
        );
        ui.add_enabled_ui(!data_busy && !pending, |ui| {
            egui::ComboBox::from_id_salt("page_size")
                .width(80.0)
                .selected_text(format!("{} rows", self.table_tabs[self.table_index].page_size))
                .show_ui(ui, |ui| {
                    for size in [25, 50, 100, 200, 500] {
                        ui.selectable_value(&mut self.table_tabs[self.table_index].page_size, size, size.to_string());
                    }
                });
            egui::ComboBox::from_id_salt("nulls_order")
                .width(110.0)
                .selected_text(self.table_tabs[self.table_index].nulls.label())
                .show_ui(ui, |ui| {
                    for order in NullsOrder::ALL {
                        ui.selectable_value(&mut self.table_tabs[self.table_index].nulls, order, order.label());
                    }
                })
                .response
                .on_hover_text("Default follows PostgreSQL: NULLs last when ascending, first when descending. Default lets indexes serve the sort.");
        });
        if old
            != (
                self.table_tabs[self.table_index].page_size,
                self.table_tabs[self.table_index].nulls,
            )
        {
            self.browse(0);
        }
        if self.table_tabs[self.table_index]
            .structure
            .columns
            .iter()
            .any(|c| c.data_type == "bytea")
        {
            egui::ComboBox::from_id_salt("binary_display")
                .width(80.0)
                .selected_text(self.table_tabs[self.table_index].binary.label())
                .show_ui(ui, |ui| {
                    for mode in display::BinaryDisplay::ALL {
                        ui.selectable_value(
                            &mut self.table_tabs[self.table_index].binary,
                            mode,
                            mode.label(),
                        );
                    }
                })
                .response
                .on_hover_text("How bytea values are shown. Stored values do not change.");
        }
        let mut hidden = self.hidden_columns();
        let before = hidden.clone();
        ui.menu_button(
            if hidden.is_empty() {
                "Columns".to_owned()
            } else {
                format!("Columns ({} hidden)", hidden.len())
            },
            |ui| {
                if ui.button("Show all").clicked() {
                    hidden.clear();
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for column in &self.table_tabs[self.table_index].structure.columns {
                            let mut visible = !hidden.contains(&column.name);
                            if ui.checkbox(&mut visible, &column.name).changed() {
                                if visible {
                                    hidden.remove(&column.name);
                                } else {
                                    hidden.insert(column.name.clone());
                                }
                            }
                        }
                    });
            },
        );
        if hidden != before {
            self.set_hidden_columns(hidden);
        }
        ui.menu_button("Import / Export", |ui| {
            if ui
                .add_enabled(
                    !data_busy
                        && !pending
                        && self.table_tabs[self.table_index]
                            .selected_relation
                            .as_ref()
                            .is_some_and(Relation::is_table),
                    egui::Button::new("Import CSV..."),
                )
                .clicked()
            {
                ui.close();
                self.choose_csv_import();
            }
            ui.separator();
            if ui.button("Export page as CSV...").clicked() {
                ui.close();
                self.export_result(&self.table_tabs[self.table_index].table_page.result.clone());
            }
            if ui
                .add_enabled(
                    !data_busy,
                    egui::Button::new("Export all matching rows as CSV..."),
                )
                .on_hover_text(
                    "Streams every row that matches the filter, in the current sort order",
                )
                .clicked()
            {
                ui.close();
                self.export_table();
            }
            if ui.button("Copy page as Markdown").clicked() {
                ui.ctx().copy_text(sql::markdown(
                    &self.table_tabs[self.table_index].table_page.result,
                ));
                ui.close();
            }
        });
    }

    fn content_view(&mut self, ui: &mut egui::Ui, data_busy: bool) {
        if self.table_tabs[self.table_index].show_details {
            egui::Panel::right("row_details")
                .default_size(330.0)
                .min_size(220.0)
                .resizable(true)
                .frame(egui::Frame::new().fill(theme::sidebar()).inner_margin(12))
                .show(ui, |ui| self.details_panel(ui));
        }
        let id = self.table_tabs[self.table_index]
            .selected_relation
            .as_ref()
            .map(Relation::qualified_name)
            .unwrap_or_default();
        let hidden = self.hidden_columns();
        let styles: Vec<ColumnStyle> = self.table_tabs[self.table_index]
            .structure
            .columns
            .iter()
            .map(|c| ColumnStyle {
                hidden: hidden.contains(&c.name),
                binary: c.data_type == "bytea",
                primary_key: c.primary_key,
                foreign_key: self.table_tabs[self.table_index]
                    .structure
                    .foreign_keys
                    .iter()
                    .any(|key| key.columns.contains(&c.name)),
                nullable: c.nullable,
            })
            .collect();
        let is_table = self.table_tabs[self.table_index]
            .selected_relation
            .as_ref()
            .is_some_and(Relation::is_table);
        let editable = self.can_edit();
        let empty = if data_busy {
            "Loading..."
        } else {
            "No rows loaded. Refresh to try again."
        };
        let tab = &mut self.table_tabs[self.table_index];
        let source = StagedRows {
            result: &tab.table_page.result,
            edits: &tab.edits,
            columns: &tab.structure.columns,
            editable,
            insertable: is_table,
        };
        let mut request = None;
        let structure = &tab.structure;
        let relations = &self.relations;
        // While a new page loads, the previous rows stay visible but inactive.
        let content = &mut tab.content;
        let options = grid::GridOptions {
            sort: &tab.sort,
            sortable: !data_busy && tab.edits.is_empty(),
            deletable: is_table && !data_busy && self.dialog.is_none(),
            empty_message: empty,
            styles: &styles,
            binary: tab.binary,
        };
        let action = ui
            .add_enabled_ui(!tab.loading_rows, |ui| {
                grid::show(
                    ui,
                    ("content", &id),
                    &source,
                    content,
                    options,
                    &mut |ui, row, column| {
                        cell_links(ui, &source, structure, relations, row, column, &mut request);
                    },
                )
            })
            .inner;
        let visible: Vec<usize> = (0..source.result.columns.len())
            .filter(|c| !styles[*c].hidden)
            .collect();
        let rows_of = |indices: &[usize]| -> Vec<Row> {
            indices
                .iter()
                .map(|&row| {
                    visible
                        .iter()
                        .map(|&column| source.cell(row, column).text().map(str::to_owned))
                        .collect()
                })
                .collect()
        };
        let mut selected: Vec<usize> = tab.content.selection.indices().collect();
        if selected.is_empty()
            && let Some(row) = tab.content.current_row()
        {
            selected.push(row);
        }
        let names: Vec<String> = visible
            .iter()
            .map(|&c| source.result.columns[c].clone())
            .collect();
        let mut clipboard = None;
        if let Some(format) = action.copy
            && !selected.is_empty()
        {
            let rows = rows_of(&selected);
            let refs: Vec<&Row> = rows.iter().collect();
            clipboard = Some(match format {
                CopyFormat::Tsv => sql::tsv_rows(None, &refs),
                CopyFormat::Csv => sql::csv_rows(&names, &refs),
                CopyFormat::Json => sql::json_rows(&names, &refs),
                CopyFormat::Markdown => sql::markdown_rows(&names, &refs),
                CopyFormat::Insert => {
                    let insertable: Vec<usize> = (0..visible.len())
                        .filter(|i| !tab.structure.columns[visible[*i]].generated)
                        .collect();
                    let columns: Vec<String> =
                        insertable.iter().map(|i| names[*i].clone()).collect();
                    let values: Vec<Vec<Option<&str>>> = rows
                        .iter()
                        .map(|row| insertable.iter().map(|i| row[*i].as_deref()).collect())
                        .collect();
                    tab.selected_relation
                        .as_ref()
                        .map(|relation| sql::insert_statements(relation, &columns, &values))
                        .unwrap_or_default()
                }
            });
        }
        if let Some(column) = action.copy_column {
            let values: Vec<Row> = (0..source.row_count())
                .map(|row| vec![source.cell(row, column).text().map(str::to_owned)])
                .collect();
            clipboard = Some(sql::tsv_rows(None, &values.iter().collect::<Vec<_>>()));
        }
        let pasted = action.paste.as_ref().map(|text| {
            let mut rows = sql::parse_tsv(text);
            if rows.first().is_some_and(|first| {
                first.len() == names.len()
                    && first
                        .iter()
                        .zip(&names)
                        .all(|(a, b)| a.as_deref() == Some(b.as_str()))
            }) {
                rows.remove(0);
            }
            rows
        });
        let inspect = action.edit.map(|(row, column)| {
            let value = source.cell(row, column);
            (
                row,
                column,
                value.text().unwrap_or_default().to_owned(),
                value == CellValue::Null,
                source.editable(row, column) && source.row_state(row) != RowState::Deleted,
            )
        });
        if let Some(text) = clipboard {
            ui.ctx().copy_text(text);
            self.status = "Copied to the clipboard".into();
        }
        if let Some((row, column, value)) = action.commit {
            tab.edits
                .set(&tab.table_page.result.rows, row, column, value);
        }
        if let Some((row, column)) = action.set_default {
            let loaded = tab.table_page.result.rows.len();
            tab.edits.set_default(loaded, row, column);
        }
        if action.delete {
            self.stage_delete();
        }
        if action.restore {
            let rows: Vec<usize> = self.table_tabs[self.table_index]
                .content
                .selection
                .indices()
                .collect();
            self.table_tabs[self.table_index].edits.restore(rows);
        }
        if let Some(column) = action.hide_column {
            let mut hidden = self.hidden_columns();
            hidden.insert(
                self.table_tabs[self.table_index].structure.columns[column]
                    .name
                    .clone(),
            );
            self.set_hidden_columns(hidden);
        }
        if let Some(rows) = pasted {
            self.paste_rows(rows, &visible);
        }
        if let Some((row, column, value, null, editable)) = inspect {
            self.dialog = Some(Dialog::Cell {
                row,
                column,
                name: self.table_tabs[self.table_index].table_page.result.columns[column].clone(),
                value,
                null,
                editable,
                pretty: false,
            });
        }
        if let Some((column, additive)) = action.sort {
            let descending = self.table_tabs[self.table_index]
                .sort
                .iter()
                .find(|c| c.name == column)
                .is_some_and(|c| !c.descending);
            if !additive {
                self.table_tabs[self.table_index].sort.clear();
            }
            self.table_tabs[self.table_index]
                .sort
                .retain(|c| c.name != column);
            self.table_tabs[self.table_index].sort.push(SortColumn {
                name: column,
                descending,
            });
            self.browse(0);
        }
        if let Some((column, descending)) = action.sort_direction {
            self.table_tabs[self.table_index].sort = vec![SortColumn {
                name: column,
                descending,
            }];
            self.browse(0);
        }
        match request {
            Some(CellRequest::Filter(filter)) => {
                let mut filter_set = self.table_tabs[self.table_index].applied_filter.clone();
                filter_set.any = false;
                filter_set.conditions.push(filter);
                self.table_tabs[self.table_index].filter = filter_set.clone();
                self.table_tabs[self.table_index].show_filter = true;
                self.apply_filter(filter_set);
            }
            Some(CellRequest::Open(relation, filter)) => self.open_related(relation, filter),
            Some(CellRequest::PickKey(row, column, key)) => self.pick_key(row, column, key),
            None => {}
        }
        if !data_busy
            && self.table_tabs[self.table_index]
                .table_page
                .result
                .rows
                .is_empty()
            && self.table_tabs[self.table_index].edits.inserts.is_empty()
            && !self.table_tabs[self.table_index]
                .structure
                .columns
                .is_empty()
        {
            ui.label(RichText::new("No matching rows.").color(theme::muted()));
        }
    }

    /// Adds pasted rows as new rows, mapping fields to the visible columns in order.
    fn paste_rows(&mut self, rows: Vec<Row>, visible: &[usize]) {
        if rows.is_empty()
            || rows
                .iter()
                .all(|row| row.iter().all(|v| v.as_deref() == Some("")))
        {
            return;
        }
        if !self.table_tabs[self.table_index]
            .selected_relation
            .as_ref()
            .is_some_and(Relation::is_table)
        {
            self.warning = Some("Rows can only be pasted into a base table.".into());
            return;
        }
        let targets: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|c| !self.table_tabs[self.table_index].structure.columns[*c].generated)
            .collect();
        if let Some(row) = rows.iter().find(|row| row.len() > targets.len()) {
            self.warning = Some(format!(
                "Pasted rows have {} values, but only {} editable columns are visible.",
                row.len(),
                targets.len()
            ));
            return;
        }
        let loaded = self.table_tabs[self.table_index]
            .table_page
            .result
            .rows
            .len();
        let tab = &mut self.table_tabs[self.table_index];
        let count = rows.len();
        for values in rows {
            let index = tab.edits.add_row(tab.structure.columns.len());
            for (column, value) in targets.iter().zip(values) {
                tab.edits
                    .set(&tab.table_page.result.rows, loaded + index, *column, value);
            }
        }
        self.status = format!("Pasted {count} new rows. Save to insert them.");
    }

    fn details_panel(&mut self, ui: &mut egui::Ui) {
        let loaded = self.table_tabs[self.table_index]
            .table_page
            .result
            .rows
            .len();
        let Some(row) = self.table_tabs[self.table_index]
            .content
            .current_row()
            .filter(|row| *row < loaded + self.table_tabs[self.table_index].edits.inserts.len())
        else {
            ui.label(RichText::new("Select a row to see its values.").color(theme::muted()));
            return;
        };
        let editable_rows = self.can_edit();
        let is_table = self.table_tabs[self.table_index]
            .selected_relation
            .as_ref()
            .is_some_and(Relation::is_table);
        let inserted = row >= loaded;
        let deleted = self.table_tabs[self.table_index].edits.is_deleted(row);
        ui.horizontal(|ui| {
            ui.heading(if inserted {
                "New row".to_owned()
            } else {
                format!("Row {}", row + 1)
            });
            if deleted {
                ui.label(
                    RichText::new("marked for deletion")
                        .color(egui::Color32::from_rgb(223, 113, 111)),
                );
            }
        });
        ui.add_space(6.0);
        let mut changes: Vec<(usize, Option<Option<String>>)> = Vec::new();
        let mut open = None;
        let mut pick: Option<ForeignKey> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (index, column) in self.table_tabs[self.table_index]
                    .structure
                    .columns
                    .iter()
                    .enumerate()
                {
                    let source = StagedRows {
                        result: &self.table_tabs[self.table_index].table_page.result,
                        edits: &self.table_tabs[self.table_index].edits,
                        columns: &self.table_tabs[self.table_index].structure.columns,
                        editable: editable_rows,
                        insertable: is_table,
                    };
                    let value = source.cell(row, index);
                    let editable = source.editable(row, index) && !deleted;
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong(&column.name);
                            ui.label(
                                RichText::new(&column.data_type)
                                    .small()
                                    .color(theme::muted()),
                            );
                            if column.primary_key {
                                ui.label(RichText::new("PRIMARY").small().color(theme::gold()));
                            }
                        });
                        let mut text = value.text().unwrap_or_default().to_owned();
                        let mut null = value == CellValue::Null;
                        let mut default = value == CellValue::Default;
                        if editable {
                            ui.horizontal(|ui| {
                                if column.nullable && ui.checkbox(&mut null, "NULL").changed() {
                                    changes.push((index, Some((!null).then(|| text.clone()))));
                                }
                                if inserted && ui.checkbox(&mut default, "DEFAULT").changed() {
                                    changes.push((
                                        index,
                                        if default {
                                            None
                                        } else {
                                            Some(Some(String::new()))
                                        },
                                    ));
                                }
                                if matches!(column.data_type.as_str(), "json" | "jsonb")
                                    && !null
                                    && !default
                                    && ui.small_button("Format JSON").clicked()
                                    && let Some(pretty) = display::pretty_json(&text)
                                {
                                    changes.push((index, Some(Some(pretty))));
                                }
                            });
                            let lines = text.lines().count().clamp(1, 8);
                            let response = ui.add_enabled(
                                !null && !default,
                                egui::TextEdit::multiline(&mut text)
                                    .desired_rows(lines)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(if default {
                                        "DEFAULT"
                                    } else if null {
                                        "NULL"
                                    } else {
                                        ""
                                    }),
                            );
                            if response.changed() {
                                changes.push((index, Some(Some(text.clone()))));
                            }
                        } else {
                            let mut shown = match value {
                                CellValue::Text(text) if column.data_type == "bytea" => {
                                    display::binary(text, self.table_tabs[self.table_index].binary)
                                        .into_owned()
                                }
                                CellValue::Text(text) => text.to_owned(),
                                CellValue::Null => "NULL".into(),
                                CellValue::Default => "DEFAULT".into(),
                            };
                            let lines = shown.lines().count().clamp(1, 8);
                            ui.add(
                                egui::TextEdit::multiline(&mut shown)
                                    .desired_rows(lines)
                                    .desired_width(f32::INFINITY)
                                    .interactive(true)
                                    .text_color(if value.text().is_none() {
                                        theme::muted()
                                    } else {
                                        ui.visuals().text_color()
                                    }),
                            );
                        }
                        for key in self.table_tabs[self.table_index]
                            .structure
                            .foreign_keys
                            .iter()
                            .filter(|key| key.columns.contains(&column.name))
                        {
                            let values: Option<Vec<String>> = key
                                .columns
                                .iter()
                                .map(|name| {
                                    let position = self.table_tabs[self.table_index]
                                        .structure
                                        .columns
                                        .iter()
                                        .position(|c| &c.name == name)?;
                                    source.cell(row, position).text().map(str::to_owned)
                                })
                                .collect();
                            if editable
                                && key.columns.len() == 1
                                && ui
                                    .small_button("Choose...")
                                    .on_hover_text("Search the referenced table")
                                    .clicked()
                            {
                                pick = Some(key.clone());
                            }
                            if ui
                                .add_enabled(
                                    values.is_some() && !inserted,
                                    egui::Button::new(format!(
                                        "Open {}.{} →",
                                        key.referenced_schema, key.referenced_table
                                    ))
                                    .small(),
                                )
                                .clicked()
                                && let Some(values) = values
                            {
                                open = Some((
                                    relation_named(
                                        &self.relations,
                                        &key.referenced_schema,
                                        &key.referenced_table,
                                    ),
                                    equality_filter(&key.referenced_columns, values),
                                ));
                            }
                        }
                        ui.add_space(8.0);
                    });
                }
            });
        let tab = &mut self.table_tabs[self.table_index];
        let rows = &tab.table_page.result.rows;
        for (column, change) in changes {
            match change {
                Some(value) => tab.edits.set(rows, row, column, value),
                None => tab.edits.set_default(loaded, row, column),
            }
        }
        if let Some((relation, filter)) = open {
            self.open_related(relation, filter);
        }
        if let Some(key) = pick
            && let Some(column) = self.table_tabs[self.table_index]
                .structure
                .columns
                .iter()
                .position(|c| key.columns.first() == Some(&c.name))
        {
            self.pick_key(row, column, key);
        }
    }

    pub(super) fn pick_key(&mut self, row: usize, column: usize, key: ForeignKey) {
        self.dialog = Some(Dialog::KeyPicker {
            row,
            column,
            key,
            search: String::new(),
            requested: None,
            results: Vec::new(),
        });
    }

    fn export_table(&mut self) {
        let Some(request) = self.browse_request(0) else {
            return;
        };
        let name = format!("{}.csv", request.relation.name);
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name(name)
            .save_file()
        {
            self.send(Command::ExportTable(request, path), "Exporting rows...");
        }
    }

    fn choose_csv_import(&mut self) {
        if self.blocked_by_edits() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv", "tsv", "txt"])
            .pick_file()
        else {
            return;
        };
        let line = match read_first_line(&path) {
            Ok(line) => line,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        let delimiter = sql::guess_delimiter(&line);
        let fields = sql::parse_csv_record(&line, delimiter);
        let mut draft = CsvDraft {
            path,
            fields,
            header: true,
            delimiter,
            mapping: Vec::new(),
        };
        self.map_csv_fields(&mut draft);
        self.dialog = Some(Dialog::ImportCsv(draft));
    }

    /// Maps each file field to a table column: by name when the file has a header,
    /// otherwise in table order.
    pub(super) fn map_csv_fields(&self, draft: &mut CsvDraft) {
        let columns: Vec<&Column> = self.table_tabs[self.table_index]
            .structure
            .columns
            .iter()
            .filter(|c| !c.generated)
            .collect();
        draft.mapping = draft
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                if draft.header {
                    columns
                        .iter()
                        .find(|c| c.name == field.trim())
                        .or_else(|| {
                            columns
                                .iter()
                                .find(|c| c.name.eq_ignore_ascii_case(field.trim()))
                        })
                        .map(|c| c.name.clone())
                        .unwrap_or_default()
                } else {
                    columns
                        .get(index)
                        .map(|c| c.name.clone())
                        .unwrap_or_default()
                }
            })
            .collect();
    }

    pub(super) fn start_csv_import(&mut self, draft: &CsvDraft) -> Result<()> {
        let relation = self.table_tabs[self.table_index]
            .selected_relation
            .clone()
            .context("Select a table first.")?;
        anyhow::ensure!(
            draft.mapping.iter().all(|c| !c.is_empty()),
            "Choose a table column for every field in the file."
        );
        let mut seen = BTreeSet::new();
        anyhow::ensure!(
            draft.mapping.iter().all(|c| seen.insert(c)),
            "Each table column can receive only one field."
        );
        self.send(
            Command::ImportCsv(CsvImport {
                relation,
                columns: draft.mapping.clone(),
                path: draft.path.clone(),
                header: draft.header,
                delimiter: draft.delimiter,
            }),
            "Importing CSV...",
        );
        Ok(())
    }

    fn add_filter_condition(&mut self) {
        let column = self.table_tabs[self.table_index]
            .structure
            .columns
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.table_tabs[self.table_index]
            .filter
            .conditions
            .push(Filter {
                column,
                ..Default::default()
            });
    }

    fn filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.separator();
        let mut remove = None;
        let mut apply = false;
        let mut clear = false;
        ui.add_enabled_ui(
            !self.busy.blocks(Lane::Data)
                && !self.table_tabs[self.table_index]
                    .structure
                    .columns
                    .is_empty(),
            |ui| {
                let tab = &mut self.table_tabs[self.table_index];
                for (index, condition) in tab.filter.conditions.iter_mut().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("filter_column")
                                .selected_text(&condition.column)
                                .show_ui(ui, |ui| {
                                    for column in &tab.structure.columns {
                                        ui.selectable_value(
                                            &mut condition.column,
                                            column.name.clone(),
                                            &column.name,
                                        );
                                    }
                                });
                            egui::ComboBox::from_id_salt("filter_operator")
                                .selected_text(condition.operator.label())
                                .show_ui(ui, |ui| {
                                    for operator in FilterOperator::ALL {
                                        ui.selectable_value(
                                            &mut condition.operator,
                                            operator,
                                            operator.label(),
                                        );
                                    }
                                });
                            if condition.operator.needs_value() {
                                let response = if condition.operator.is_list() {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut condition.value)
                                            .desired_rows(3)
                                            .desired_width(240.0)
                                            .hint_text("One value per line"),
                                    )
                                } else {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut condition.value)
                                            .hint_text("Value")
                                            .desired_width(240.0),
                                    )
                                };
                                if response.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    && !condition.operator.is_list()
                                {
                                    apply = true;
                                }
                            }
                            if ui
                                .small_button("−")
                                .on_hover_text("Remove condition")
                                .clicked()
                            {
                                remove = Some(index);
                            }
                        });
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("+ Condition").clicked() {
                        self.add_filter_condition();
                    }
                    if self.table_tabs[self.table_index].filter.conditions.len() > 1 {
                        egui::ComboBox::from_id_salt("filter_match")
                            .selected_text(if self.table_tabs[self.table_index].filter.any {
                                "Match any"
                            } else {
                                "Match all"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.table_tabs[self.table_index].filter.any,
                                    false,
                                    "Match all",
                                );
                                ui.selectable_value(
                                    &mut self.table_tabs[self.table_index].filter.any,
                                    true,
                                    "Match any",
                                );
                            });
                    }
                    ui.label(RichText::new("SQL").color(theme::muted()));
                    ui.add(
                        egui::TextEdit::singleline(
                            &mut self.table_tabs[self.table_index].filter.custom_sql,
                        )
                        .hint_text(
                            "Optional expression, e.g. created_at > now() - interval '1 day'",
                        )
                        .desired_width(340.0)
                        .font(egui::TextStyle::Monospace),
                    )
                    .on_hover_text(
                        "A SQL boolean expression added with AND. Use column names directly.",
                    );
                    if theme::primary(ui, "Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Clear").clicked() {
                        clear = true;
                    }
                });
            },
        );
        if let Some(index) = remove {
            self.table_tabs[self.table_index]
                .filter
                .conditions
                .remove(index);
        }
        if clear {
            self.table_tabs[self.table_index].filter = FilterSet::default();
            self.add_filter_condition();
            self.apply_filter(FilterSet::default());
        } else if apply {
            let mut filter = self.table_tabs[self.table_index].filter.clone();
            filter.conditions.retain(|c| {
                !c.operator.needs_value()
                    || !c.value.is_empty()
                    || matches!(c.operator, FilterOperator::Equal | FilterOperator::NotEqual)
            });
            self.apply_filter(filter);
        }
    }
}

/// Context menu items that depend on the table: quick filters and foreign keys.
fn cell_links(
    ui: &mut egui::Ui,
    source: &StagedRows<'_>,
    structure: &TableStructure,
    relations: &[Relation],
    row: usize,
    column: usize,
    request: &mut Option<CellRequest>,
) {
    let Some(name) = structure.columns.get(column).map(|c| c.name.clone()) else {
        return;
    };
    let value = source.cell(row, column);
    if value != CellValue::Default && source.row_state(row) != RowState::Inserted {
        let shown: String = match value {
            CellValue::Text(text) => {
                let short: String = text.chars().take(30).collect();
                if short.len() < text.len() {
                    format!("'{short}…'")
                } else {
                    format!("'{short}'")
                }
            }
            _ => "NULL".into(),
        };
        let (show, hide) = match value.text() {
            Some(text) => (
                (FilterOperator::Equal, text.to_owned(), format!("= {shown}")),
                (
                    FilterOperator::NotEqual,
                    text.to_owned(),
                    format!("≠ {shown}"),
                ),
            ),
            None => (
                (FilterOperator::IsNull, String::new(), "is NULL".into()),
                (
                    FilterOperator::IsNotNull,
                    String::new(),
                    "is not NULL".into(),
                ),
            ),
        };
        for (operator, value, label) in [show, hide] {
            if ui.button(format!("Filter: {name} {label}")).clicked() {
                *request = Some(CellRequest::Filter(Filter {
                    column: name.clone(),
                    operator,
                    value,
                }));
                ui.close();
            }
        }
        ui.separator();
    }
    let value_of = |names: &[String]| -> Option<Vec<String>> {
        names
            .iter()
            .map(|name| {
                let index = structure.columns.iter().position(|c| &c.name == name)?;
                source.cell(row, index).text().map(str::to_owned)
            })
            .collect()
    };
    let mut linked = false;
    for key in structure
        .foreign_keys
        .iter()
        .filter(|key| key.columns.contains(&name))
    {
        if key.columns.len() == 1
            && source.editable(row, column)
            && source.row_state(row) != RowState::Deleted
            && ui
                .button(format!(
                    "Choose a row from {}.{}...",
                    key.referenced_schema, key.referenced_table
                ))
                .clicked()
        {
            *request = Some(CellRequest::PickKey(row, column, key.clone()));
            ui.close();
        }
        let values = value_of(&key.columns);
        if ui
            .add_enabled(
                values.is_some(),
                egui::Button::new(format!(
                    "Open referenced row in {}.{}",
                    key.referenced_schema, key.referenced_table
                )),
            )
            .on_disabled_hover_text("The foreign key value is NULL.")
            .clicked()
            && let Some(values) = values
        {
            *request = Some(CellRequest::Open(
                relation_named(relations, &key.referenced_schema, &key.referenced_table),
                equality_filter(&key.referenced_columns, values),
            ));
            ui.close();
        }
        linked = true;
    }
    for key in structure
        .referenced_by
        .iter()
        .filter(|key| key.referenced_columns.contains(&name))
    {
        let values = value_of(&key.referenced_columns);
        if ui
            .add_enabled(
                values.is_some() && source.row_state(row) != RowState::Inserted,
                egui::Button::new(format!(
                    "Show rows in {}.{} referencing this ({})",
                    key.schema,
                    key.table,
                    key.columns.join(", ")
                )),
            )
            .clicked()
            && let Some(values) = values
        {
            *request = Some(CellRequest::Open(
                relation_named(relations, &key.schema, &key.table),
                equality_filter(&key.columns, values),
            ));
            ui.close();
        }
        linked = true;
    }
    if linked {
        ui.separator();
    }
}

/// The catalog entry for a table, or a base-table placeholder when the catalog
/// was not refreshed since the table was created.
fn relation_named(relations: &[Relation], schema: &str, name: &str) -> Relation {
    relations
        .iter()
        .find(|r| r.schema == schema && r.name == name)
        .cloned()
        .unwrap_or_else(|| Relation {
            schema: schema.into(),
            name: name.into(),
            kind: "r".into(),
            ..Default::default()
        })
}

fn equality_filter(columns: &[String], values: Vec<String>) -> FilterSet {
    FilterSet {
        conditions: columns
            .iter()
            .zip(values)
            .map(|(column, value)| Filter {
                column: column.clone(),
                operator: FilterOperator::Equal,
                value,
            })
            .collect(),
        ..Default::default()
    }
}

fn read_first_line(path: &std::path::Path) -> Result<String> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path)?;
    let mut line = String::new();
    std::io::BufReader::new(file)
        .take(1024 * 1024)
        .read_line(&mut line)
        .context("The file is not UTF-8 text.")?;
    anyhow::ensure!(!line.trim().is_empty(), "The file is empty.");
    Ok(line.trim_start_matches('\u{feff}').to_owned())
}

/// Groups digits in threes with spaces, as in 56 000.
pub(super) fn group(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(digit);
    }
    if value < 0 {
        out.insert(0, '-');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn groups_thousands() {
        assert_eq!(super::group(7), "7");
        assert_eq!(super::group(56_000), "56 000");
        assert_eq!(super::group(1_234_567), "1 234 567");
        assert_eq!(super::group(-1000), "-1 000");
    }
}
