use super::*;

#[cfg(test)]
mod tests;

/// Independent browsing and editing state for one table tab.
pub(super) struct TableTab {
    pub id: Uuid,
    pub mode: Mode,
    pub needs_load: bool,
    pub table_editor: Option<TableEditor>,
    pub selected_relation: Option<Relation>,
    pub structure: TableStructure,
    pub table_page: TablePage,
    pub page: usize,
    pub pending_page: usize,
    pub page_size: usize,
    pub sort: Vec<SortColumn>,
    pub nulls: NullsOrder,
    /// A page request is running; the previous rows stay visible but inactive.
    pub loading_rows: bool,
    /// The filter being edited and the filter applied to the current rows.
    pub filter: FilterSet,
    pub applied_filter: FilterSet,
    pub show_filter: bool,
    pub row_count: Option<i64>,
    /// A row count is waiting for the shared count session.
    pub count_wanted: bool,
    /// Grid state and staged changes of the table browser.
    pub content: grid::GridState,
    pub edits: PendingEdits,
    pub binary: BinaryDisplay,
    pub show_details: bool,
    /// Tables visited through foreign keys, for Back.
    pub back_stack: Vec<BrowseRequest>,
}

impl Default for TableTab {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            mode: Mode::Content,
            needs_load: false,
            table_editor: None,
            selected_relation: None,
            structure: TableStructure::default(),
            table_page: TablePage::default(),
            page: 0,
            pending_page: 0,
            page_size: 100,
            sort: vec![],
            nulls: NullsOrder::Default,
            loading_rows: false,
            filter: FilterSet::default(),
            applied_filter: FilterSet::default(),
            show_filter: false,
            row_count: None,
            count_wanted: false,
            content: grid::GridState::default(),
            edits: PendingEdits::default(),
            binary: BinaryDisplay::default(),
            show_details: false,
            back_stack: Vec::new(),
        }
    }
}

impl TableTab {
    fn is_open(&self) -> bool {
        self.selected_relation.is_some() || self.table_editor.is_some()
    }

    pub(super) fn has_changes(&self) -> bool {
        self.has_row_changes()
            || self
                .table_editor
                .as_ref()
                .is_some_and(|editor| editor.creating || editor.original != editor.edited)
    }

    fn has_row_changes(&self) -> bool {
        !self.edits.is_empty()
            || self
                .content
                .editor
                .as_ref()
                .is_some_and(|editor| editor.change().is_some())
    }

    fn title(&self) -> String {
        self.selected_relation.as_ref().map_or_else(
            || "New table".into(),
            |relation| format!("{}.{}", relation.schema, relation.name),
        )
    }
}

impl PosternApp {
    pub(super) fn reopen_table_after_ddl(&mut self) {
        let owner = self.ddl_tab.take();
        let Some((schema, name)) = self.reopen_after_catalog.take() else {
            return;
        };
        let Some(relation) = self
            .relations
            .iter()
            .find(|r| r.schema == schema && r.name == name)
            .cloned()
        else {
            return;
        };
        let visible = self.table_tabs[self.table_index].id;
        let mode = self.mode;
        if let Some(index) = self.table_tabs.iter().position(|tab| Some(tab.id) == owner) {
            self.table_index = index;
            self.table_tabs[index].selected_relation = Some(relation);
            self.table_tabs[index].mode = Mode::Structure;
            self.mode = Mode::Structure;
        } else {
            self.select_relation(relation);
        }
        // A new tab already has a load queued or in progress.
        if !self.table_tabs[self.table_index].loading_rows {
            self.refresh();
        }
        let showing_owner = owner == Some(visible)
            && matches!(
                mode,
                Mode::Content | Mode::Structure | Mode::Definition | Mode::NewTable
            );
        if !showing_owner
            && let Some(index) = self
                .table_tabs
                .iter()
                .position(|tab| tab.id == visible && tab.is_open())
        {
            self.table_index = index;
            self.mode = mode;
        }
    }

    pub(super) fn stage_inline_edit(&mut self) {
        let tab = &mut self.table_tabs[self.table_index];
        if let Some(editor) = tab.content.editor.take()
            && let Some(value) = editor.change()
        {
            tab.edits.set(
                &tab.table_page.result.rows,
                editor.row,
                editor.column,
                value,
            );
        }
    }

    pub(super) fn cancel_table_editor(&mut self) {
        let tab = &mut self.table_tabs[self.table_index];
        let creating = tab
            .table_editor
            .as_ref()
            .is_some_and(|editor| editor.creating);
        tab.table_editor = None;
        if creating {
            let id = tab.id;
            self.close_table(id);
        }
    }

    pub(super) fn reconcile_table_tabs(&mut self) {
        // Keep dirty tabs available for review even if another session dropped the table.
        let missing: Vec<Uuid> = self
            .table_tabs
            .iter()
            .filter(|tab| {
                !tab.has_changes()
                    && tab.selected_relation.as_ref().is_some_and(|selected| {
                        !self
                            .relations
                            .iter()
                            .any(|r| r.schema == selected.schema && r.name == selected.name)
                    })
            })
            .map(|tab| tab.id)
            .collect();
        for id in missing {
            self.close_table(id);
        }
    }

    pub(super) fn table_tabs_bar(&mut self, ui: &mut egui::Ui) {
        if !self.table_tabs.iter().any(TableTab::is_open) {
            return;
        }
        let mut select = None;
        let mut close = None;
        egui::Panel::top("table_tabs")
            .frame(egui::Frame::new().fill(theme::toolbar()).inner_margin(6))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("table_tabs_scroll")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!self.busy.control && self.dialog.is_none(), |ui| {
                                for (index, tab) in self
                                    .table_tabs
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, tab)| tab.is_open())
                                {
                                    ui.push_id(tab.id, |ui| {
                                        let mut title = tab.title();
                                        if tab.has_changes() {
                                            title.push_str(" •");
                                        }
                                        if tab.loading_rows {
                                            title.push_str(" …");
                                        }
                                        let selected = index == self.table_index
                                            && matches!(
                                                self.mode,
                                                Mode::Content
                                                    | Mode::Structure
                                                    | Mode::Definition
                                                    | Mode::NewTable
                                            );
                                        if ui.selectable_label(selected, title).clicked() {
                                            select = Some(index);
                                        }
                                        if ui
                                            .small_button("×")
                                            .on_hover_text("Close table (Command-W)")
                                            .clicked()
                                        {
                                            close = Some(tab.id);
                                        }
                                        ui.separator();
                                    });
                                }
                            });
                        });
                    });
            });
        if let Some(index) = select {
            self.select_table_tab(index);
        }
        if let Some(id) = close {
            self.request_close_table(id);
        }
    }

    pub(super) fn add_table_tab(&mut self, tab: TableTab) {
        if !self.table_tabs[self.table_index].is_open() {
            self.table_tabs[self.table_index] = tab;
        } else {
            self.table_tabs.push(tab);
            self.table_index = self.table_tabs.len() - 1;
        }
        self.mode = self.table_tabs[self.table_index].mode;
    }

    pub(super) fn select_table_tab(&mut self, index: usize) {
        if index >= self.table_tabs.len() || self.busy.control || self.dialog.is_some() {
            return;
        }
        self.table_index = index;
        self.mode = self.table_tabs[index].mode;
        self.warning = None;
    }

    pub(super) fn request_close_table(&mut self, id: Uuid) {
        if self.busy.control || self.dialog.is_some() {
            return;
        }
        if (self.busy.data && self.data_tab == Some(id)) || self.ddl_tab == Some(id) {
            self.warning = Some("This table has an operation running. Wait for it to finish or cancel it before closing.".into());
            return;
        }
        let Some(tab) = self.table_tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        if tab.has_changes() {
            self.dialog = Some(Dialog::CloseTable(id));
        } else {
            self.close_table(id);
        }
    }

    pub(super) fn close_table(&mut self, id: Uuid) {
        if self.busy.control
            || (self.busy.data && self.data_tab == Some(id))
            || self.ddl_tab == Some(id)
        {
            return;
        }
        let Some(index) = self.table_tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let current = self.table_tabs[self.table_index].id;
        self.table_tabs.remove(index);
        if self.table_tabs.is_empty() {
            self.table_tabs.push(TableTab::default());
        }
        self.table_index = self
            .table_tabs
            .iter()
            .position(|tab| tab.id == current)
            .unwrap_or_else(|| index.min(self.table_tabs.len() - 1));
        if id == current
            && matches!(
                self.mode,
                Mode::Content | Mode::Structure | Mode::Definition | Mode::NewTable
            )
        {
            self.mode = if self.table_tabs[self.table_index].is_open() {
                self.table_tabs[self.table_index].mode
            } else {
                Mode::Query
            };
        }
        if self.count_tab == Some(id) && self.busy.sql.contains(&COUNT_SESSION) {
            self.worker.cancel_lane(Lane::Sql(COUNT_SESSION));
        }
    }

    pub(super) fn clear_table_tabs(&mut self) {
        self.table_tabs = vec![TableTab::default()];
        self.table_index = 0;
        self.data_tab = None;
        self.count_tab = None;
        self.ddl_tab = None;
        self.reopen_after_catalog = None;
    }

    pub(super) fn blocked_by_any_edits(&mut self) -> bool {
        let Some(tab) = self.table_tabs.iter().find(|tab| tab.has_changes()) else {
            return false;
        };
        self.warning = Some(format!(
            "Save or discard pending changes in {} first.",
            tab.title()
        ));
        true
    }

    pub(super) fn blocked_by_any_row_edits(&mut self) -> bool {
        let Some(tab) = self.table_tabs.iter().find(|tab| tab.has_row_changes()) else {
            return false;
        };
        self.warning = Some(format!(
            "Save or discard pending row changes in {} first.",
            tab.title()
        ));
        true
    }

    /// Data commands stay serialized, but users can switch tabs while they run.
    pub(super) fn load_waiting_table(&mut self) {
        if self.connected.is_none() || self.busy.blocks(Lane::Data) {
            return;
        }
        let next = if self.table_tabs[self.table_index].needs_load {
            Some(self.table_index)
        } else {
            self.table_tabs.iter().position(|tab| tab.needs_load)
        };
        let Some(index) = next else {
            return;
        };
        let visible = self.table_index;
        self.table_index = index;
        if let Some(relation) = self.table_tabs[index].selected_relation.clone() {
            self.table_tabs[index].loading_rows = self.send(
                Command::Load(relation, self.table_tabs[index].page_size),
                "Loading table...",
            );
        }
        self.table_tabs[index].needs_load = false;
        self.table_index = visible;
    }

    pub(super) fn count_waiting_table(&mut self) {
        if self.connected.is_none() || self.busy.blocks(Lane::Sql(COUNT_SESSION)) {
            return;
        }
        if let Some(index) = self.table_tabs.iter().position(|tab| tab.count_wanted) {
            let visible = self.table_index;
            self.table_index = index;
            self.request_count(true);
            self.table_index = visible;
        }
    }
}
