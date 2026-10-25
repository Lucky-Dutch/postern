//! The SQL editor: line numbers, error locations, find and replace, schema-aware
//! completion, and synchronization with files on disk.

use super::*;
use crate::sql::complete::{self, ItemKind};
use egui::text::{CCursor, CCursorRange};
use egui_extras::syntax_highlighting::highlight;
use std::{hash::Hasher, time::SystemTime};

/// The open completion list.
pub(super) struct CompletionPopup {
    document: Uuid,
    items: Vec<complete::CompletionItem>,
    selected: usize,
    /// Byte range of the word the chosen item replaces.
    replace: Range<usize>,
    anchor: egui::Pos2,
    /// Where the list was drawn last frame.
    rect: Option<egui::Rect>,
}

#[derive(Default)]
pub(super) struct FindBar {
    pub(super) query: String,
    replacement: String,
    match_case: bool,
    current: Option<usize>,
    focus: bool,
}

/// What Postern last read from or wrote to a document's file.
pub(super) struct FileState {
    modified: Option<SystemTime>,
    /// Hash of the file's text at that time, to tell whether the editor differs.
    saved: u64,
}

pub(super) fn text_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(text.as_bytes());
    hasher.finish()
}

/// Byte ranges of every occurrence of `query`, optionally ignoring case.
pub(super) fn find_matches(text: &str, query: &str, match_case: bool) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    if match_case {
        return text
            .match_indices(query)
            .map(|(start, found)| start..start + found.len())
            .collect();
    }
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut matches = Vec::new();
    let mut indices = text.char_indices().peekable();
    while let Some((start, _)) = indices.peek().copied() {
        let mut candidate = text[start..].chars().flat_map(char::to_lowercase);
        if needle.iter().all(|c| candidate.next() == Some(*c)) {
            let mut end = start;
            let mut folded = 0;
            for c in text[start..].chars() {
                if folded >= needle.len() {
                    break;
                }
                folded += c.to_lowercase().count();
                end += c.len_utf8();
            }
            if folded == needle.len() {
                matches.push(start..end);
                while indices.peek().is_some_and(|(index, _)| *index < end) {
                    indices.next();
                }
                continue;
            }
        }
        indices.next();
    }
    matches
}

impl PosternApp {
    pub(super) fn editor(&mut self, ui: &mut egui::Ui) {
        let document_id = self.documents[self.document_index].id;
        let editor_id = egui::Id::unique(("sql_editor", document_id));
        self.find_bar(ui, editor_id);
        self.file_banner(ui);
        let focused = ui.memory(|memory| memory.has_focus(editor_id));
        // Keys for the completion list are taken before the editor sees them.
        let mut accept = None;
        if let Some(popup) = &mut self.completion {
            if popup.document != document_id || !focused {
                let over_popup = popup.rect.is_some_and(|rect| {
                    ui.ctx()
                        .pointer_hover_pos()
                        .is_some_and(|pos| rect.contains(pos))
                });
                if !focused && !over_popup {
                    self.completion = None;
                }
            } else {
                ui.input_mut(|input| {
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        popup.selected = (popup.selected + 1).min(popup.items.len() - 1);
                    }
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        popup.selected = popup.selected.saturating_sub(1);
                    }
                    if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                        || input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                    {
                        accept = Some(popup.selected);
                    }
                });
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    self.completion = None;
                }
            }
        }
        if let Some(index) = accept {
            self.accept_completion(ui.ctx(), editor_id, index);
        }
        let explicit =
            focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Space));
        let matches = self
            .find
            .as_ref()
            .map(|find| {
                find_matches(
                    &self.documents[self.document_index].sql,
                    &find.query,
                    find.match_case,
                )
            })
            .unwrap_or_default();
        let current_match = self.find.as_ref().and_then(|find| find.current);
        let document = &mut self.documents[self.document_index];
        let theme = theme::code_theme(15.0);
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _width: f32| {
            let job = highlight(ui.ctx(), ui.style(), &theme, text.as_str(), "sql");
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let lines = document.sql.split('\n').count();
        let gutter = ui.fonts_mut(|f| f.glyph_width(&font, '0'))
            * lines.to_string().len().max(2) as f32
            + 14.0;
        let previous_cursor = self.editor_cursor;
        let mut changed = false;
        let mut anchor = None;
        egui::ScrollArea::both()
            .id_salt(("editor_scroll", document.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(14).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        let (gutter_rect, _) = ui.allocate_exact_size(
                            egui::vec2(gutter, ui.available_height()),
                            egui::Sense::hover(),
                        );
                        let output = egui::TextEdit::multiline(&mut document.sql)
                            .id(editor_id)
                            .code_editor()
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(12)
                            .min_size(egui::vec2(ui.available_width(), ui.available_height()))
                            .frame(egui::Frame::NONE)
                            .layouter(&mut layouter)
                            .show(ui);
                        let painter = ui.painter();
                        let mut line = 1;
                        let mut starts_line = true;
                        for row in &output.galley.rows {
                            if starts_line {
                                painter.text(
                                    egui::pos2(
                                        gutter_rect.right() - 8.0,
                                        output.galley_pos.y + row.rect().min.y,
                                    ),
                                    egui::Align2::RIGHT_TOP,
                                    line.to_string(),
                                    font.clone(),
                                    theme::muted().gamma_multiply(0.7),
                                );
                                line += 1;
                            }
                            starts_line = row.ends_with_newline;
                        }
                        let rect_of = |range: &Range<usize>| {
                            let start = lexer::byte_to_char(&document.sql, range.start);
                            let end = lexer::byte_to_char(&document.sql, range.end);
                            let from = output.galley.pos_from_cursor(CCursor::new(start));
                            let to = output.galley.pos_from_cursor(CCursor::new(end));
                            egui::Rect::from_min_max(
                                from.min,
                                egui::pos2(to.max.x.max(from.min.x + 2.0), from.max.y),
                            )
                            .translate(output.galley_pos.to_vec2())
                        };
                        for (index, range) in matches.iter().enumerate() {
                            let color = if Some(index) == current_match {
                                egui::Color32::from_rgba_unmultiplied(230, 160, 60, 110)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(230, 200, 80, 45)
                            };
                            painter.rect_filled(rect_of(range), 2, color);
                        }
                        if let Some(index) = current_match
                            && let Some(range) = matches.get(index)
                            && self.find.as_ref().is_some_and(|find| find.focus)
                        {
                            ui.scroll_to_rect(rect_of(range), Some(egui::Align::Center));
                        }
                        changed = output.response.changed();
                        if changed {
                            self.dirty_since = Some(Instant::now());
                        }
                        if let Some(range) = output.cursor_range {
                            let sorted = range.as_sorted_char_range();
                            self.editor_selection = sorted.start.0..sorted.end.0;
                            self.editor_cursor = range.primary.index.0;
                            let cursor = output
                                .galley
                                .pos_from_cursor(range.primary)
                                .translate(output.galley_pos.to_vec2());
                            anchor = Some(cursor.left_bottom() + egui::vec2(0.0, 2.0));
                        }
                        let selection = self
                            .pending_cursor
                            .filter(|(id, _)| *id == document.id)
                            .map(|(_, offset)| {
                                let byte = lexer::char_to_byte(&document.sql, offset);
                                lexer::tokenize(&document.sql)
                                    .into_iter()
                                    .find(|t| !t.is_trivia() && t.start <= byte && byte < t.end())
                                    .map_or(offset..offset, |t| {
                                        lexer::byte_to_char(&document.sql, t.start)
                                            ..lexer::byte_to_char(&document.sql, t.end())
                                    })
                            })
                            .or_else(|| {
                                self.pending_select
                                    .clone()
                                    .filter(|(id, _)| *id == document.id)
                                    .map(|(_, range)| range)
                            });
                        if let Some(range) = selection {
                            let focus = self.pending_cursor.is_some();
                            self.pending_cursor = None;
                            self.pending_select = None;
                            let mut state = output.state.clone();
                            state.cursor.set_char_range(Some(CCursorRange::two(
                                CCursor::new(range.start),
                                CCursor::new(range.end),
                            )));
                            state.store(ui.ctx(), editor_id);
                            let rect = output
                                .galley
                                .pos_from_cursor(CCursor::new(range.start))
                                .translate(output.galley_pos.to_vec2());
                            ui.scroll_to_rect(rect, Some(egui::Align::Center));
                            if focus {
                                output.response.request_focus();
                            }
                        }
                    });
                });
            });
        if let Some(find) = &mut self.find {
            find.focus = false;
        }
        // Typing an identifier character or a dot opens or updates completions;
        // moving the cursor any other way closes them.
        let document = &self.documents[self.document_index];
        let cursor_byte = lexer::char_to_byte(&document.sql, self.editor_cursor);
        let typed = changed
            && document.sql[..cursor_byte]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '"');
        if explicit || typed {
            match complete::complete(
                &document.sql,
                cursor_byte,
                &self.completion_catalog,
                explicit,
            ) {
                Some(completion) => {
                    let selected = self
                        .completion
                        .as_ref()
                        .filter(|popup| popup.replace.start == completion.replace.start)
                        .map_or(0, |popup| popup.selected.min(completion.items.len() - 1));
                    self.completion = Some(CompletionPopup {
                        document: document.id,
                        items: completion.items,
                        selected,
                        replace: completion.replace,
                        anchor: anchor.unwrap_or_default(),
                        rect: None,
                    });
                }
                None => self.completion = None,
            }
        } else if changed || (self.editor_cursor != previous_cursor && self.completion.is_some()) {
            self.completion = None;
        }
        if let Some(anchor) = anchor {
            self.editor_caret = Some(anchor);
            if let Some(popup) = &mut self.completion {
                popup.anchor = anchor;
            }
        }
        self.completion_popup(ui.ctx(), editor_id);
    }

    fn completion_popup(&mut self, ctx: &egui::Context, editor_id: egui::Id) {
        let Some(popup) = &self.completion else {
            return;
        };
        let mut clicked = None;
        let area = egui::Area::new(egui::Id::unique("sql_completion"))
            .fixed_pos(popup.anchor)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(260.0);
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (index, item) in popup.items.iter().enumerate() {
                                let kind = match item.kind {
                                    ItemKind::Column => "column",
                                    ItemKind::Table => "table",
                                    ItemKind::Schema => "schema",
                                    ItemKind::Function => "function",
                                    ItemKind::Keyword => "keyword",
                                };
                                let detail = if item.detail.is_empty() {
                                    kind.to_owned()
                                } else {
                                    format!("{kind} · {}", item.detail)
                                };
                                let response = ui.horizontal(|ui| {
                                    let response = ui.selectable_label(
                                        index == popup.selected,
                                        RichText::new(&item.label).monospace(),
                                    );
                                    ui.label(RichText::new(detail).small().color(theme::muted()));
                                    response
                                });
                                if index == popup.selected {
                                    response.inner.scroll_to_me(None);
                                }
                                if response.inner.clicked() {
                                    clicked = Some(index);
                                }
                            }
                        });
                });
            });
        if let Some(popup) = &mut self.completion {
            popup.rect = Some(area.response.rect);
        }
        if let Some(index) = clicked {
            self.accept_completion(ctx, editor_id, index);
            ctx.memory_mut(|memory| memory.request_focus(editor_id));
        }
    }

    fn accept_completion(&mut self, ctx: &egui::Context, editor_id: egui::Id, index: usize) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(item) = popup.items.get(index) else {
            return;
        };
        let Some(document) = self.documents.iter_mut().find(|d| d.id == popup.document) else {
            return;
        };
        if popup.replace.end > document.sql.len()
            || !document.sql.is_char_boundary(popup.replace.start)
        {
            return;
        }
        document
            .sql
            .replace_range(popup.replace.clone(), &item.insert);
        let cursor = lexer::byte_to_char(&document.sql, popup.replace.start + item.insert.len());
        if let Some(mut state) = egui::TextEdit::load_state(ctx, editor_id) {
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
            state.store(ctx, editor_id);
        }
        self.editor_cursor = cursor;
        self.editor_selection = cursor..cursor;
        self.dirty_since = Some(Instant::now());
    }

    /// Opens completions at the editor cursor, as Control-Space does.
    #[cfg(feature = "smoke")]
    pub(super) fn complete_at_cursor(&mut self) {
        let document = &self.documents[self.document_index];
        let cursor = lexer::char_to_byte(&document.sql, self.editor_cursor);
        self.completion = complete::complete(&document.sql, cursor, &self.completion_catalog, true)
            .map(|completion| CompletionPopup {
                document: document.id,
                items: completion.items,
                selected: 0,
                replace: completion.replace,
                anchor: self.editor_caret.unwrap_or_default(),
                rect: None,
            });
    }

    pub(super) fn open_find(&mut self) {
        let find = self.find.get_or_insert_with(FindBar::default);
        find.focus = true;
        let document = &self.documents[self.document_index];
        let start = lexer::char_to_byte(&document.sql, self.editor_selection.start);
        let end = lexer::char_to_byte(&document.sql, self.editor_selection.end);
        if start < end && !document.sql[start..end].contains('\n') {
            find.query = document.sql[start..end].to_owned();
        }
    }

    fn find_bar(&mut self, ui: &mut egui::Ui, editor_id: egui::Id) {
        let Some(find) = &mut self.find else {
            return;
        };
        let document = &mut self.documents[self.document_index];
        let mut close = false;
        let mut step: Option<bool> = None;
        let mut replace_one = false;
        let mut replace_all = false;
        egui::Panel::top("find_bar")
            .frame(egui::Frame::new().fill(theme::sidebar()).inner_margin(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut find.query)
                            .id(egui::Id::unique("find_query"))
                            .hint_text("Find")
                            .desired_width(240.0),
                    );
                    if find.focus {
                        response.request_focus();
                    }
                    if response.changed() {
                        find.current = None;
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        step = Some(!ui.input(|i| i.modifiers.shift));
                        response.request_focus();
                    }
                    if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                    let count = find_matches(&document.sql, &find.query, find.match_case).len();
                    ui.label(
                        RichText::new(match (count, find.current) {
                            (0, _) if !find.query.is_empty() => "No matches".to_owned(),
                            (0, _) => String::new(),
                            (count, Some(current)) => format!("{} of {count}", current + 1),
                            (count, None) => format!("{count} matches"),
                        })
                        .small()
                        .color(theme::muted()),
                    );
                    if ui
                        .small_button("‹")
                        .on_hover_text("Previous (Command-Shift-G)")
                        .clicked()
                    {
                        step = Some(false);
                    }
                    if ui
                        .small_button("›")
                        .on_hover_text("Next (Command-G)")
                        .clicked()
                    {
                        step = Some(true);
                    }
                    ui.checkbox(&mut find.match_case, "Match case");
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut find.replacement)
                            .hint_text("Replace with")
                            .desired_width(200.0),
                    );
                    if ui.small_button("Replace").clicked() {
                        replace_one = true;
                    }
                    if ui.small_button("Replace all").clicked() {
                        replace_all = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Done").clicked() {
                            close = true;
                        }
                    });
                });
            });
        if let Some(forward) = self.pending_find_step.take() {
            step = Some(forward);
        } else if ui.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::G,
            )
        }) {
            step = Some(false);
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::G)) {
            step = Some(true);
        }
        let matches = find_matches(&document.sql, &find.query, find.match_case);
        if replace_all && !matches.is_empty() {
            for range in matches.iter().rev() {
                document.sql.replace_range(range.clone(), &find.replacement);
            }
            self.status = format!("Replaced {} occurrences", matches.len());
            find.current = None;
            self.dirty_since = Some(Instant::now());
        } else if replace_one {
            if let Some(range) = find.current.and_then(|index| matches.get(index)).cloned() {
                document.sql.replace_range(range.clone(), &find.replacement);
                self.dirty_since = Some(Instant::now());
                let after = range.start + find.replacement.len();
                let next = find_matches(&document.sql, &find.query, find.match_case)
                    .iter()
                    .position(|m| m.start >= after);
                find.current = next;
                find.focus = true;
            } else {
                step = Some(true);
            }
        }
        if let Some(forward) = step {
            let matches = find_matches(&document.sql, &find.query, find.match_case);
            if !matches.is_empty() {
                let cursor = lexer::char_to_byte(&document.sql, self.editor_cursor);
                let next = match (find.current, forward) {
                    (Some(current), true) => (current + 1) % matches.len(),
                    (Some(current), false) => (current + matches.len() - 1) % matches.len(),
                    (None, true) => matches.iter().position(|m| m.start >= cursor).unwrap_or(0),
                    (None, false) => matches
                        .iter()
                        .rposition(|m| m.end <= cursor)
                        .unwrap_or(matches.len() - 1),
                };
                find.current = Some(next);
                find.focus = true;
                let range = &matches[next];
                self.pending_select = Some((
                    document.id,
                    lexer::byte_to_char(&document.sql, range.start)
                        ..lexer::byte_to_char(&document.sql, range.end),
                ));
            }
        }
        if close {
            self.find = None;
            ui.memory_mut(|memory| memory.request_focus(editor_id));
        }
    }

    /// Records the file text a document was loaded from or saved to.
    pub(super) fn remember_file(&mut self, id: Uuid, path: &std::path::Path, text: &str) {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        self.file_states.insert(
            id,
            FileState {
                modified,
                saved: text_hash(text),
            },
        );
        self.file_conflicts.remove(&id);
    }

    /// True when a file-backed document differs from what is on disk.
    pub(super) fn unsaved(&self, document: &QueryDocument) -> bool {
        document.path.is_some()
            && self
                .file_states
                .get(&document.id)
                .is_some_and(|state| state.saved != text_hash(&document.sql))
    }

    /// Checks file-backed documents for changes on disk. Unchanged documents are
    /// reloaded; documents with unsaved edits are flagged as conflicts.
    pub(super) fn sync_files(&mut self) {
        if self.last_file_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_file_check = Instant::now();
        let mut reloaded = Vec::new();
        for document in &mut self.documents {
            let Some(path) = &document.path else {
                continue;
            };
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            let Some(state) = self.file_states.get_mut(&document.id) else {
                if let Ok(text) = std::fs::read_to_string(path) {
                    if text != document.sql {
                        self.file_conflicts.insert(document.id);
                    }
                    self.file_states.insert(
                        document.id,
                        FileState {
                            modified,
                            saved: text_hash(&text),
                        },
                    );
                }
                continue;
            };
            if modified.is_none() || modified == state.modified {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            state.modified = modified;
            let disk = text_hash(&text);
            if disk == state.saved {
                continue;
            }
            if text_hash(&document.sql) == state.saved {
                document.sql = text;
                state.saved = disk;
                reloaded.push(document.title.clone());
            } else {
                self.file_conflicts.insert(document.id);
            }
        }
        if !reloaded.is_empty() {
            self.status = format!("Reloaded {} from disk", reloaded.join(", "));
            self.dirty_since = Some(Instant::now());
        }
    }

    fn file_banner(&mut self, ui: &mut egui::Ui) {
        let document = &self.documents[self.document_index];
        if !self.file_conflicts.contains(&document.id) {
            return;
        }
        let id = document.id;
        let path = document.path.clone();
        let mut reload = false;
        let mut keep = false;
        egui::Panel::top("file_conflict")
            .frame(
                egui::Frame::new()
                    .fill(theme::warning_fill())
                    .inner_margin(8),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("The file on disk differs from this query.");
                    if ui.button("Reload from disk").clicked() {
                        reload = true;
                    }
                    if ui.button("Keep this version").clicked() {
                        keep = true;
                    }
                });
            });
        if reload && let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    self.remember_file(id, &path, &text);
                    self.documents[self.document_index].sql = text;
                    self.dirty_since = Some(Instant::now());
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        } else if keep {
            self.file_conflicts.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_matches_with_and_without_case() {
        assert_eq!(
            find_matches("Select select SELECT", "select", true),
            vec![Range { start: 7, end: 13 }]
        );
        assert_eq!(
            find_matches("Select select SELECT", "select", false),
            [0..6, 7..13, 14..20]
        );
        assert_eq!(find_matches("aaaa", "aa", false), [0..2, 2..4]);
        assert_eq!(
            find_matches("Zażółć ZAŻÓŁĆ", "żółć", false),
            [2..10, 13..21]
        );
        assert!(find_matches("abc", "", false).is_empty());
    }
}
