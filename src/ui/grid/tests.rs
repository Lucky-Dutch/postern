use super::*;

#[test]
fn shift_range_keeps_its_anchor_and_context_click_preserves_the_selection() {
    let mut selection = RowSelection::default();
    selection.click(4, false);
    selection.click(1, true);
    assert_eq!(selection.indices().collect::<Vec<_>>(), [1, 2, 3, 4]);
    selection.click(6, true);
    assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 5, 6]);
    selection.context_click(5);
    assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 5, 6]);
    selection.context_click(2);
    assert_eq!(selection.indices().collect::<Vec<_>>(), [2]);
    selection.clear();
    selection.click(5, true);
    assert_eq!(selection.indices().collect::<Vec<_>>(), [5]);
}

/// A result whose cells are editable, for inline editing tests.
struct Editable(QueryResult);

impl GridSource for Editable {
    fn columns(&self) -> &[String] {
        &self.0.columns
    }
    fn row_count(&self) -> usize {
        self.0.rows.len()
    }
    fn cell(&self, row: usize, column: usize) -> CellValue<'_> {
        self.0.cell(row, column)
    }
    fn editable(&self, _row: usize, _column: usize) -> bool {
        true
    }
}

#[derive(Default)]
struct GridHarness {
    ctx: egui::Context,
    state: GridState,
    editable: bool,
    text: String,
    time: f64,
    labels: std::collections::HashMap<String, egui::Pos2>,
    text_rect: Option<egui::Rect>,
}

impl GridHarness {
    fn frame(&mut self, events: Vec<egui::Event>, deletable: bool) -> GridAction {
        self.time += 0.1;
        let mut action = GridAction::default();
        let result = QueryResult {
            columns: vec!["value".into(), "other".into()],
            rows: (0..7)
                .map(|i| vec![Some(format!("row_{i}")), Some(format!("other_{i}"))])
                .collect(),
            ..Default::default()
        };
        let editable = Editable(result.clone());
        let source: &dyn GridSource = if self.editable { &editable } else { &result };
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ui| {
                self.text_rect = Some(ui.text_edit_singleline(&mut self.text).rect);
                action = show(
                    ui,
                    "test_grid",
                    source,
                    &mut self.state,
                    GridOptions {
                        sortable: true,
                        deletable,
                        ..Default::default()
                    },
                    &mut |_, _, _| {},
                );
            },
        );
        fn labels(shape: &egui::Shape, output: &mut std::collections::HashMap<String, egui::Pos2>) {
            match shape {
                egui::Shape::Text(text) => {
                    output.insert(
                        text.galley.text().to_owned(),
                        text.pos + text.galley.size() / 2.0,
                    );
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        labels(shape, output);
                    }
                }
                _ => {}
            }
        }
        self.labels.clear();
        for shape in &output.shapes {
            labels(&shape.shape, &mut self.labels);
        }
        output.drop_without_applying_deltas();
        action
    }

    fn click(&mut self, pos: egui::Pos2, button: egui::PointerButton, shift: bool) -> GridAction {
        let modifiers = egui::Modifiers {
            shift,
            ..Default::default()
        };
        self.frame(
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed: true,
                    modifiers,
                },
            ],
            true,
        );
        self.frame(
            vec![egui::Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers,
            }],
            true,
        )
    }

    fn key(&mut self, key: egui::Key, deletable: bool) -> GridAction {
        self.key_with(key, egui::Modifiers::NONE, deletable)
    }

    fn key_with(
        &mut self,
        key: egui::Key,
        modifiers: egui::Modifiers,
        deletable: bool,
    ) -> GridAction {
        self.frame(
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ],
            deletable,
        )
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![egui::Event::Text(text.into())], true);
    }
}

#[test]
fn grid_handles_shift_click_and_context_delete_for_the_whole_range() {
    let mut grid = GridHarness::default();
    grid.frame(vec![], true);
    grid.frame(vec![], true);
    grid.click(grid.labels["row_1"], egui::PointerButton::Primary, false);
    grid.click(grid.labels["row_4"], egui::PointerButton::Primary, true);
    assert_eq!(
        grid.state.selection.indices().collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    grid.click(grid.labels["row_2"], egui::PointerButton::Secondary, false);
    grid.frame(vec![], true);
    assert_eq!(grid.state.selection.len(), 4);
    let delete = grid.labels["Delete 4 rows"];
    assert!(
        grid.click(delete, egui::PointerButton::Primary, false)
            .delete
    );
    grid.frame(vec![], true);
    grid.click(grid.labels["row_6"], egui::PointerButton::Secondary, false);
    assert_eq!(grid.state.selection.indices().collect::<Vec<_>>(), [6]);
}

#[test]
fn delete_keys_require_an_editable_grid_with_keyboard_focus() {
    let mut grid = GridHarness::default();
    grid.frame(vec![], true);
    grid.frame(vec![], true);
    grid.click(grid.labels["row_1"], egui::PointerButton::Primary, false);
    grid.click(grid.labels["row_3"], egui::PointerButton::Primary, true);
    assert!(grid.key(egui::Key::Backspace, true).delete);
    assert!(grid.key(egui::Key::Delete, true).delete);
    assert!(!grid.key(egui::Key::Delete, false).delete);
    grid.click(
        grid.text_rect.unwrap().center(),
        egui::PointerButton::Primary,
        false,
    );
    assert!(!grid.key(egui::Key::Backspace, true).delete);
    assert!(!grid.key(egui::Key::Delete, true).delete);
    assert_eq!(grid.state.selection.len(), 3);
}

#[test]
fn arrow_keys_move_the_cell_and_shift_extends_the_selection() {
    let mut grid = GridHarness::default();
    grid.frame(vec![], true);
    grid.frame(vec![], true);
    grid.click(grid.labels["row_2"], egui::PointerButton::Primary, false);
    assert_eq!(grid.state.cell, Some((2, 0)));
    grid.key(egui::Key::ArrowDown, true);
    grid.key(egui::Key::ArrowRight, true);
    assert_eq!(grid.state.cell, Some((3, 1)));
    grid.key_with(egui::Key::ArrowDown, egui::Modifiers::SHIFT, true);
    assert_eq!(grid.state.selection.indices().collect::<Vec<_>>(), [3, 4]);
    let action = grid.frame(vec![egui::Event::Copy], true);
    assert_eq!(action.copy, Some(CopyFormat::Tsv));
    let action = grid.frame(vec![egui::Event::Paste("a\tb".into())], true);
    assert_eq!(action.paste.as_deref(), Some("a\tb"));
}

#[test]
fn enter_edits_inline_and_escape_cancels() {
    let mut grid = GridHarness {
        editable: true,
        ..Default::default()
    };
    grid.frame(vec![], true);
    grid.frame(vec![], true);
    grid.click(grid.labels["row_1"], egui::PointerButton::Primary, false);
    grid.key(egui::Key::Enter, true);
    assert!(grid.state.editor.is_some());
    grid.frame(vec![], true);
    grid.type_text("!");
    let action = grid.key(egui::Key::Enter, true);
    assert_eq!(action.commit, Some((1, 0, Some("row_1!".into()))));
    assert!(grid.state.editor.is_none());
    grid.key(egui::Key::Enter, true);
    grid.frame(vec![], true);
    grid.type_text("x");
    let action = grid.key(egui::Key::Escape, true);
    assert!(action.commit.is_none());
    assert!(grid.state.editor.is_none());
}

#[test]
fn read_only_cells_open_the_inspector() {
    let mut grid = GridHarness::default();
    grid.frame(vec![], true);
    grid.frame(vec![], true);
    grid.click(grid.labels["row_5"], egui::PointerButton::Primary, false);
    let action = grid.key(egui::Key::Enter, true);
    assert_eq!(action.edit, Some((5, 0)));
    assert!(grid.state.editor.is_none());
}

#[test]
fn an_unchanged_inline_edit_stages_nothing() {
    let mut grid = GridHarness {
        editable: true,
        ..Default::default()
    };
    grid.frame(vec![], true);
    grid.frame(vec![], true);
    grid.click(grid.labels["row_2"], egui::PointerButton::Primary, false);
    grid.key(egui::Key::Enter, true);
    assert!(grid.state.editor.is_some());
    grid.frame(vec![], true);
    let action = grid.key(egui::Key::Enter, true);
    assert!(
        action.commit.is_none(),
        "leaving a value unchanged must not stage an edit"
    );
    assert!(grid.state.editor.is_none());
}
