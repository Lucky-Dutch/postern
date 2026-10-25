//! Staged grid changes. Cell edits, new rows, and deletions are kept here until
//! the user saves them in one transaction or discards them.

use crate::models::{InsertValue, Row, RowChanges};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingEdits {
    /// Changed cells of loaded rows, by row index and column index.
    pub updates: BTreeMap<usize, BTreeMap<usize, Option<String>>>,
    /// Indices of loaded rows marked for deletion.
    pub deletes: BTreeSet<usize>,
    /// New rows, shown after the loaded rows.
    pub inserts: Vec<Vec<InsertValue>>,
}

impl PendingEdits {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.deletes.is_empty() && self.inserts.is_empty()
    }

    /// Number of rows that will change when saved.
    pub fn len(&self) -> usize {
        self.updates
            .keys()
            .filter(|row| !self.deletes.contains(row))
            .count()
            + self.deletes.len()
            + self.inserts.len()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Sets a cell of a loaded row (`row < rows.len()`) or of a new row. Setting a
    /// loaded cell back to its original value removes the change.
    pub fn set(&mut self, rows: &[Row], row: usize, column: usize, value: Option<String>) {
        if let Some(original) = rows.get(row) {
            let cells = self.updates.entry(row).or_default();
            if original.get(column) == Some(&value) {
                cells.remove(&column);
            } else {
                cells.insert(column, value);
            }
            if cells.is_empty() {
                self.updates.remove(&row);
            }
        } else if let Some(values) = self.inserts.get_mut(row - rows.len())
            && let Some(cell) = values.get_mut(column)
        {
            *cell = match value {
                Some(text) => InsertValue::Text(text),
                None => InsertValue::Null,
            };
        }
    }

    /// Resets a new row's cell to the column default.
    pub fn set_default(&mut self, loaded: usize, row: usize, column: usize) {
        if let Some(cell) = row
            .checked_sub(loaded)
            .and_then(|index| self.inserts.get_mut(index))
            .and_then(|values| values.get_mut(column))
        {
            *cell = InsertValue::Default;
        }
    }

    pub fn add_row(&mut self, columns: usize) -> usize {
        self.inserts.push(vec![InsertValue::Default; columns]);
        self.inserts.len() - 1
    }

    /// Marks loaded rows for deletion and removes new rows.
    pub fn delete(&mut self, loaded: usize, rows: impl IntoIterator<Item = usize>) {
        let mut new_rows = Vec::new();
        for row in rows {
            if row < loaded {
                self.deletes.insert(row);
            } else {
                new_rows.push(row - loaded);
            }
        }
        new_rows.sort_unstable();
        new_rows.dedup();
        for index in new_rows.into_iter().rev() {
            if index < self.inserts.len() {
                self.inserts.remove(index);
            }
        }
    }

    pub fn restore(&mut self, rows: impl IntoIterator<Item = usize>) {
        for row in rows {
            self.deletes.remove(&row);
        }
    }

    pub fn is_deleted(&self, row: usize) -> bool {
        self.deletes.contains(&row)
    }

    /// The value shown for a loaded cell, including a staged change.
    pub fn value<'a>(&'a self, rows: &'a [Row], row: usize, column: usize) -> Option<&'a str> {
        match self.updates.get(&row).and_then(|cells| cells.get(&column)) {
            Some(value) => value.as_deref(),
            None => rows.get(row)?.get(column)?.as_deref(),
        }
    }

    /// Converts to database changes. Deleted rows drop their cell edits.
    pub fn to_changes(&self, rows: &[Row]) -> RowChanges {
        RowChanges {
            updates: self
                .updates
                .iter()
                .filter(|(row, _)| !self.deletes.contains(row))
                .filter_map(|(row, cells)| {
                    Some((
                        rows.get(*row)?.clone(),
                        cells.iter().map(|(c, v)| (*c, v.clone())).collect(),
                    ))
                })
                .collect(),
            inserts: self.inserts.clone(),
            deletes: self
                .deletes
                .iter()
                .filter_map(|row| rows.get(*row).cloned())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            vec![Some("1".into()), Some("a".into())],
            vec![Some("2".into()), None],
        ]
    }

    #[test]
    fn edits_track_changes_and_drop_reverted_values() {
        let rows = rows();
        let mut edits = PendingEdits::default();
        edits.set(&rows, 0, 1, Some("b".into()));
        assert_eq!(edits.value(&rows, 0, 1), Some("b"));
        assert_eq!(edits.len(), 1);
        edits.set(&rows, 0, 1, Some("a".into()));
        assert!(edits.is_empty());
        edits.set(&rows, 1, 1, Some(String::new()));
        assert_eq!(edits.value(&rows, 1, 1), Some(""));
        edits.set(&rows, 1, 1, None);
        assert!(edits.is_empty(), "NULL back to NULL is no change");
    }

    #[test]
    fn new_rows_and_deletions_convert_to_changes() {
        let rows = rows();
        let mut edits = PendingEdits::default();
        let first = edits.add_row(2);
        edits.add_row(2);
        edits.set(&rows, rows.len() + first, 1, Some("new".into()));
        edits.set(&rows, rows.len() + 1, 0, None);
        edits.set_default(rows.len(), rows.len() + 1, 0);
        edits.set(&rows, 0, 1, Some("edited".into()));
        edits.set(&rows, 1, 1, Some("gone".into()));
        edits.delete(rows.len(), [1, rows.len() + 1]);
        let changes = edits.to_changes(&rows);
        assert_eq!(
            changes.updates,
            vec![(rows[0].clone(), vec![(1, Some("edited".into()))])]
        );
        assert_eq!(changes.deletes, vec![rows[1].clone()]);
        assert_eq!(
            changes.inserts,
            vec![vec![InsertValue::Default, InsertValue::Text("new".into())]]
        );
        assert_eq!(edits.len(), 3);
        edits.restore([1]);
        assert_eq!(edits.to_changes(&rows).updates.len(), 2);
    }
}
