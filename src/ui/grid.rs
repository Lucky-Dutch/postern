//! A virtualized data grid shared by the table browser and SQL results.
//!
//! The grid reads cells through [`GridSource`], so staged edits can be overlaid
//! on loaded rows. It supports row selection, keyboard navigation, inline editing
//! of short single-line values, hidden columns, and context menus. Changes are
//! reported as a [`GridAction`]; the caller decides what to stage or execute.

use super::{
    display::{self, BinaryDisplay},
    theme,
};
use crate::models::{QueryResult, SortColumn};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

/// Values longer than this, or with line breaks, open the full editor.
const INLINE_LIMIT: usize = 500;

#[derive(Default)]
pub struct RowSelection {
    rows: BTreeSet<usize>,
    anchor: Option<usize>,
}

impl RowSelection {
    pub fn click(&mut self, index: usize, shift: bool) {
        let anchor = if shift {
            *self.anchor.get_or_insert(index)
        } else {
            self.anchor = Some(index);
            index
        };
        self.rows = (anchor.min(index)..=anchor.max(index)).collect();
    }

    fn context_click(&mut self, index: usize) {
        if !self.rows.contains(&index) {
            self.click(index, false);
        }
    }

    pub fn select_all(&mut self, count: usize) {
        self.rows = (0..count).collect();
        self.anchor = Some(0);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn contains(&self, index: usize) -> bool {
        self.rows.contains(&index)
    }

    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.iter().copied()
    }
}

/// An inline cell editor.
pub struct InlineEditor {
    pub row: usize,
    pub column: usize,
    pub text: String,
    /// The text the editor started with. Leaving it unchanged stages nothing, so
    /// a NULL or DEFAULT cell does not become an empty string.
    original: String,
    focused: bool,
}

impl InlineEditor {
    /// The staged value, or `None` when the text was not changed.
    pub fn change(&self) -> Option<Option<String>> {
        (self.text != self.original).then(|| Some(self.text.clone()))
    }
}

/// Interaction state of one grid, kept between frames.
#[derive(Default)]
pub struct GridState {
    pub selection: RowSelection,
    /// The focused cell, used for keyboard navigation and the row details panel.
    pub cell: Option<(usize, usize)>,
    pub editor: Option<InlineEditor>,
    scroll_to: Option<usize>,
}

impl GridState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The row shown in a details panel: the focused cell's row, else the first
    /// selected row.
    pub fn current_row(&self) -> Option<usize> {
        self.cell
            .map(|(row, _)| row)
            .or_else(|| self.selection.indices().next())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellValue<'a> {
    Text(&'a str),
    Null,
    /// A new row's column that will receive its database default.
    Default,
}

impl<'a> CellValue<'a> {
    pub fn text(self) -> Option<&'a str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Null | Self::Default => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowState {
    #[default]
    Stored,
    Deleted,
    Inserted,
}

/// Rows shown by a grid.
pub trait GridSource {
    fn columns(&self) -> &[String];
    fn row_count(&self) -> usize;
    fn cell(&self, row: usize, column: usize) -> CellValue<'_>;
    fn edited(&self, _row: usize, _column: usize) -> bool {
        false
    }
    fn row_state(&self, _row: usize) -> RowState {
        RowState::Stored
    }
    fn editable(&self, _row: usize, _column: usize) -> bool {
        false
    }
}

impl GridSource for QueryResult {
    fn columns(&self) -> &[String] {
        &self.columns
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, column: usize) -> CellValue<'_> {
        match self.rows.get(row).and_then(|values| values.get(column)) {
            Some(Some(text)) => CellValue::Text(text),
            _ => CellValue::Null,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ColumnStyle {
    pub hidden: bool,
    /// A `bytea` column shown in the selected binary display.
    pub binary: bool,
    pub primary_key: bool,
    pub foreign_key: bool,
    pub nullable: bool,
}

/// How a grid renders and which interactions it offers.
#[derive(Clone, Copy)]
pub struct GridOptions<'a> {
    pub sort: &'a [SortColumn],
    pub sortable: bool,
    pub deletable: bool,
    pub empty_message: &'a str,
    /// Per-column styles by column index. Missing entries use the defaults.
    pub styles: &'a [ColumnStyle],
    pub binary: BinaryDisplay,
}

impl Default for GridOptions<'_> {
    fn default() -> Self {
        Self {
            sort: &[],
            sortable: false,
            deletable: false,
            empty_message: "No rows",
            styles: &[],
            binary: BinaryDisplay::Hex,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFormat {
    Tsv,
    Csv,
    Json,
    Markdown,
    Insert,
}

impl CopyFormat {
    pub const ALL: [Self; 5] = [
        Self::Tsv,
        Self::Csv,
        Self::Json,
        Self::Markdown,
        Self::Insert,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tsv => "Tab-separated",
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::Markdown => "Markdown",
            Self::Insert => "INSERT statements",
        }
    }
}

#[derive(Default)]
pub struct GridAction {
    /// Open the full editor or inspector for a cell.
    pub edit: Option<(usize, usize)>,
    /// An inline edit, or Set NULL, was confirmed.
    pub commit: Option<(usize, usize, Option<String>)>,
    pub set_default: Option<(usize, usize)>,
    pub sort: Option<(String, bool)>,
    /// Explicit sort direction from the header menu: column and descending.
    pub sort_direction: Option<(String, bool)>,
    pub delete: bool,
    pub restore: bool,
    /// Copy the selected rows.
    pub copy: Option<CopyFormat>,
    pub copy_column: Option<usize>,
    pub hide_column: Option<usize>,
    /// Text pasted while the grid had focus.
    pub paste: Option<String>,
}

/// Shows a grid. `menu` adds caller-specific items to a cell's context menu.
pub fn show(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    source: &dyn GridSource,
    state: &mut GridState,
    options: GridOptions<'_>,
    menu: &mut dyn FnMut(&mut egui::Ui, usize, usize),
) -> GridAction {
    let mut action = GridAction::default();
    let columns = source.columns();
    if columns.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new(options.empty_message).color(theme::muted()));
        });
        return action;
    }
    let style = |column: usize| options.styles.get(column).copied().unwrap_or_default();
    let visible: Vec<usize> = (0..columns.len()).filter(|c| !style(*c).hidden).collect();
    let rows = source.row_count();
    if let Some(editor) = &state.editor
        && (editor.row >= rows || editor.column >= columns.len())
    {
        state.editor = None;
    }
    let focus_id = ui.make_persistent_id(("grid_focus", &id));
    let focus = ui.interact(
        ui.available_rect_before_wrap(),
        focus_id,
        egui::Sense::focusable_noninteractive(),
    );
    if focus.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                focus_id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            );
        });
        if state.editor.is_none() {
            keyboard(ui, source, state, &visible, &mut action);
        }
    }
    let scroll_to = state.scroll_to.take();
    egui::ScrollArea::horizontal()
        .id_salt(("grid_scroll", &id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut table = TableBuilder::new(ui)
                .id_salt(&id)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .sense(egui::Sense::click())
                .column(Column::initial(46.0).at_least(36.0));
            if let Some(row) = scroll_to {
                table = table.scroll_to_row(row, None);
            }
            for &index in &visible {
                let max = (0..rows.min(25))
                    .filter_map(|row| source.cell(row, index).text())
                    .map(|s| s.chars().take(45).count())
                    .max()
                    .unwrap_or(10)
                    .max(columns[index].chars().count() + 2);
                table = table.column(
                    Column::initial((max as f32 * 7.2 + 24.0).clamp(110.0, 310.0))
                        .at_least(70.0)
                        .clip(true),
                );
            }
            table
                .header(31.0, |mut header| {
                    header.col(|ui| {
                        ui.label(RichText::new("#").color(theme::muted()));
                    });
                    for &index in &visible {
                        header.col(|ui| header_cell(ui, index, columns, &options, &mut action));
                    }
                })
                .body(|body| {
                    body.rows(29.0, rows, |mut row| {
                        let index = row.index();
                        let row_state = source.row_state(index);
                        row.set_selected(state.selection.contains(index));
                        let (_, number) = row.col(|ui| {
                            let label = match row_state {
                                RowState::Stored => (index + 1).to_string(),
                                RowState::Deleted => "×".into(),
                                RowState::Inserted => "+".into(),
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(label).small().color(theme::muted()),
                                )
                                .selectable(false),
                            );
                        });
                        select_row(&number, state, index, None, focus_id);
                        number.context_menu(|ui| {
                            row_menu(ui, state, &options, &mut action);
                        });
                        for &column in &visible {
                            let cell_context = CellContext {
                                source,
                                options: &options,
                                row: index,
                                column,
                                row_state,
                                focus_id,
                            };
                            let (_, cell) = row.col(|ui| {
                                draw_cell(ui, &cell_context, state, &mut action);
                            });
                            select_row(&cell, state, index, Some(column), focus_id);
                            if cell.double_clicked() {
                                begin_edit(source, state, index, column, &mut action);
                            }
                            cell.context_menu(|ui| {
                                cell_menu(ui, &cell_context, &mut action);
                                menu(ui, index, column);
                                row_menu(ui, state, &options, &mut action);
                            });
                        }
                    });
                });
        });
    if options.deletable
        && !state.selection.is_empty()
        && focus.has_focus()
        && state.editor.is_none()
    {
        action.delete |= ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
        });
    }
    action
}

/// Everything a cell needs to draw itself and its menu.
struct CellContext<'a> {
    source: &'a dyn GridSource,
    options: &'a GridOptions<'a>,
    row: usize,
    column: usize,
    row_state: RowState,
    focus_id: egui::Id,
}

impl CellContext<'_> {
    fn style(&self) -> ColumnStyle {
        self.options
            .styles
            .get(self.column)
            .copied()
            .unwrap_or_default()
    }
}

fn header_cell(
    ui: &mut egui::Ui,
    index: usize,
    columns: &[String],
    options: &GridOptions<'_>,
    action: &mut GridAction,
) {
    let name = &columns[index];
    let style = options.styles.get(index).copied().unwrap_or_default();
    let indicator = options
        .sort
        .iter()
        .position(|s| &s.name == name)
        .map(|i| {
            format!(
                " {}{}",
                if options.sort[i].descending {
                    "↓"
                } else {
                    "↑"
                },
                i + 1
            )
        })
        .unwrap_or_default();
    let key = match (style.primary_key, style.foreign_key) {
        (true, _) => "◆ ",
        (false, true) => "→ ",
        _ => "",
    };
    let response = ui.add(
        egui::Button::new(RichText::new(format!("{key}{name}{indicator}")).strong()).frame(false),
    );
    if options.sortable && response.clicked() {
        action.sort = Some((name.clone(), ui.input(|i| i.modifiers.alt)));
    }
    let hint = match (style.primary_key, style.foreign_key) {
        (true, true) => "Primary key and foreign key. ",
        (true, false) => "Primary key. ",
        (false, true) => "Foreign key. ",
        _ => "",
    };
    let response = response.on_hover_text(if options.sortable {
        format!("{hint}Click to sort. Option-click adds another sort column.")
    } else {
        format!("{hint}{name}")
    });
    response.context_menu(|ui| {
        if options.sortable {
            if ui.button("Sort ascending").clicked() {
                action.sort_direction = Some((name.clone(), false));
                ui.close();
            }
            if ui.button("Sort descending").clicked() {
                action.sort_direction = Some((name.clone(), true));
                ui.close();
            }
            ui.separator();
        }
        if ui.button("Copy column values").clicked() {
            action.copy_column = Some(index);
            ui.close();
        }
        if ui.button("Hide column").clicked() {
            action.hide_column = Some(index);
            ui.close();
        }
    });
}

fn draw_cell(
    ui: &mut egui::Ui,
    cell: &CellContext<'_>,
    state: &mut GridState,
    action: &mut GridAction,
) {
    let CellContext {
        source,
        options,
        row,
        column,
        row_state,
        focus_id,
    } = *cell;
    let tint = match row_state {
        RowState::Deleted => Some(egui::Color32::from_rgba_unmultiplied(223, 113, 111, 40)),
        RowState::Inserted => Some(egui::Color32::from_rgba_unmultiplied(92, 187, 141, 32)),
        RowState::Stored if source.edited(row, column) => {
            Some(egui::Color32::from_rgba_unmultiplied(191, 163, 100, 45))
        }
        RowState::Stored => None,
    };
    if let Some(tint) = tint {
        ui.painter().rect_filled(ui.max_rect(), 0, tint);
    }
    if state.cell == Some((row, column)) {
        ui.painter().rect_stroke(
            ui.max_rect().shrink(1.0),
            2,
            egui::Stroke::new(1.0, theme::accent()),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(editor) = &mut state.editor
        && editor.row == row
        && editor.column == column
    {
        let response = ui.add(
            egui::TextEdit::singleline(&mut editor.text)
                .desired_width(f32::INFINITY)
                .frame(egui::Frame::NONE),
        );
        if !editor.focused {
            response.request_focus();
            editor.focused = true;
        } else if response.lost_focus() {
            let (escape, tab) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Escape),
                    i.key_pressed(egui::Key::Tab),
                )
            });
            let editor = state.editor.take().expect("editor exists");
            if !escape && let Some(value) = editor.change() {
                action.commit = Some((editor.row, editor.column, value));
            }
            if tab {
                let next = (column + 1..source.columns().len()).find(|c| {
                    !options.styles.get(*c).is_some_and(|s| s.hidden) && source.editable(row, *c)
                });
                if let Some(next) = next {
                    state.cell = Some((row, next));
                    begin_edit(source, state, row, next, action);
                }
            }
            if state.editor.is_none() {
                ui.memory_mut(|memory| memory.request_focus(focus_id));
            }
        }
        return;
    }
    let value = source.cell(row, column);
    let (display, muted) = match value {
        CellValue::Text(text) if cell.style().binary => {
            (display::binary(text, options.binary).into_owned(), false)
        }
        CellValue::Text(text) => (text.to_owned(), false),
        CellValue::Null => ("NULL".into(), true),
        CellValue::Default => ("DEFAULT".into(), true),
    };
    let preview: String = display
        .chars()
        .take(180)
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut text = RichText::new(preview);
    if muted {
        text = text.italics().color(theme::muted());
    }
    if row_state == RowState::Deleted {
        text = text.strikethrough();
    }
    ui.add(egui::Label::new(text).truncate().selectable(false));
}

fn cell_menu(ui: &mut egui::Ui, cell: &CellContext<'_>, action: &mut GridAction) {
    let (row, column) = (cell.row, cell.column);
    let value = cell.source.cell(row, column);
    if ui.button("Copy value").clicked() {
        ui.ctx().copy_text(match value {
            CellValue::Text(text) => text.to_owned(),
            CellValue::Null => "NULL".into(),
            CellValue::Default => "DEFAULT".into(),
        });
        ui.close();
    }
    let editable = cell.source.editable(row, column) && cell.row_state != RowState::Deleted;
    if ui
        .button(if editable {
            "Edit in editor..."
        } else {
            "Inspect value..."
        })
        .clicked()
    {
        action.edit = Some((row, column));
        ui.close();
    }
    if editable
        && cell.style().nullable
        && value != CellValue::Null
        && ui.button("Set NULL").clicked()
    {
        action.commit = Some((row, column, None));
        ui.close();
    }
    if editable
        && cell.row_state == RowState::Inserted
        && value != CellValue::Default
        && ui.button("Set DEFAULT").clicked()
    {
        action.set_default = Some((row, column));
        ui.close();
    }
    ui.separator();
}

/// Starts inline editing for short single-line values, or asks the caller to open
/// the full editor.
fn begin_edit(
    source: &dyn GridSource,
    state: &mut GridState,
    row: usize,
    column: usize,
    action: &mut GridAction,
) {
    let value = source.cell(row, column);
    let text = value.text().unwrap_or_default();
    if source.editable(row, column)
        && source.row_state(row) != RowState::Deleted
        && text.len() <= INLINE_LIMIT
        && !text.contains(['\n', '\r'])
    {
        state.editor = Some(InlineEditor {
            row,
            column,
            text: text.to_owned(),
            original: text.to_owned(),
            focused: false,
        });
    } else {
        action.edit = Some((row, column));
    }
}

fn row_menu(
    ui: &mut egui::Ui,
    state: &GridState,
    options: &GridOptions<'_>,
    action: &mut GridAction,
) {
    let count = state.selection.len();
    let noun = if count == 1 { "row" } else { "rows" };
    ui.menu_button(format!("Copy {count} {noun} as"), |ui| {
        for format in CopyFormat::ALL {
            if ui.button(format.label()).clicked() {
                action.copy = Some(format);
                ui.close();
            }
        }
    });
    if options.deletable {
        ui.separator();
        if ui
            .add_enabled(
                count > 0,
                egui::Button::new(format!("Delete {count} {noun}")),
            )
            .clicked()
        {
            action.delete = true;
            ui.close();
        }
        if ui
            .add_enabled(
                count > 0,
                egui::Button::new(format!("Restore {count} {noun}")),
            )
            .on_hover_text("Undo a pending deletion")
            .clicked()
        {
            action.restore = true;
            ui.close();
        }
    }
}

fn keyboard(
    ui: &mut egui::Ui,
    source: &dyn GridSource,
    state: &mut GridState,
    visible: &[usize],
    action: &mut GridAction,
) {
    let rows = source.row_count();
    if rows == 0 || visible.is_empty() {
        return;
    }
    let (row, column) = state
        .cell
        .unwrap_or((state.selection.indices().next().unwrap_or(0), visible[0]));
    let row = row.min(rows - 1);
    let position = visible.iter().position(|c| *c == column).unwrap_or(0);
    let mut moved = None;
    let mut edit = false;
    ui.input_mut(|input| {
        let shift = input.modifiers.shift;
        let vertical = if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        if input.consume_key(vertical, egui::Key::ArrowDown) {
            moved = Some((
                row.saturating_add(1).min(rows - 1),
                visible[position],
                shift,
            ));
        }
        if input.consume_key(vertical, egui::Key::ArrowUp) {
            moved = Some((row.saturating_sub(1), visible[position], shift));
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            moved = Some((row, visible[(position + 1).min(visible.len() - 1)], false));
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            moved = Some((row, visible[position.saturating_sub(1)], false));
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            edit = true;
        }
        if input.consume_key(egui::Modifiers::COMMAND, egui::Key::A) {
            state.selection.select_all(rows);
        }
        for event in &input.events {
            match event {
                egui::Event::Copy => action.copy = Some(CopyFormat::Tsv),
                egui::Event::Paste(text) => action.paste = Some(text.clone()),
                _ => {}
            }
        }
    });
    if let Some((row, column, extend)) = moved {
        state.cell = Some((row, column));
        state.selection.click(row, extend);
        state.scroll_to = Some(row);
    }
    if edit {
        let (row, column) = state.cell.unwrap_or((row, visible[position]));
        state.cell = Some((row, column));
        begin_edit(source, state, row, column, action);
    }
}

fn select_row(
    response: &egui::Response,
    state: &mut GridState,
    index: usize,
    column: Option<usize>,
    focus_id: egui::Id,
) {
    if response.clicked() {
        state
            .selection
            .click(index, response.ctx.input(|i| i.modifiers.shift));
        if let Some(column) = column {
            state.cell = Some((index, column));
        }
        response.ctx.memory_mut(|m| m.request_focus(focus_id));
        response.ctx.request_repaint();
    } else if response.secondary_clicked() {
        state.selection.context_click(index);
        if let Some(column) = column {
            state.cell = Some((index, column));
        }
        response.ctx.memory_mut(|m| m.request_focus(focus_id));
        response.ctx.request_repaint();
    }
}
