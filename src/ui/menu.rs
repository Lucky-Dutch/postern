//! The native macOS menu bar.
//!
//! Menu items carry the same shortcuts as the keyboard handler. AppKit delivers
//! a menu shortcut to the menu instead of the window, so both paths call the same
//! action. Clipboard and text editing keys are left to the window, which handles
//! them in text fields and grids.

use super::app::Action;
use crate::models::Appearance;
use eframe::egui;
use muda::{
    AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};
use std::{collections::HashMap, sync::mpsc};

pub(crate) struct NativeMenu {
    _menu: Menu,
    actions: HashMap<MenuId, Action>,
    events: mpsc::Receiver<MenuEvent>,
}

impl NativeMenu {
    /// Installs the menu bar. Returns `None` when AppKit refuses it, in which case
    /// keyboard shortcuts still work through the window.
    pub(crate) fn install(ctx: &egui::Context) -> Option<Self> {
        let mut actions = HashMap::new();
        let mut item = |label: &str, action: Action, shortcut: Option<Accelerator>| {
            let item = MenuItem::new(label, true, shortcut);
            actions.insert(item.id().clone(), action);
            item
        };
        let command = Modifiers::META;
        let command_shift = Modifiers::META | Modifiers::SHIFT;
        let key = |modifiers: Modifiers, code: Code| Some(Accelerator::new(modifiers, code));

        let new_query = item("New Query", Action::NewQuery, key(command, Code::KeyT));
        let open = item(
            "Open SQL File...",
            Action::OpenFile,
            key(command, Code::KeyO),
        );
        let save = item("Save", Action::Save, key(command, Code::KeyS));
        let save_as = item("Save As...", Action::SaveAs, key(command_shift, Code::KeyS));
        let close = item("Close Query", Action::CloseQuery, key(command, Code::KeyW));
        let find = item("Find...", Action::Find, key(command, Code::KeyF));
        let find_next = item("Find Next", Action::FindNext, key(command, Code::KeyG));
        let find_previous = item(
            "Find Previous",
            Action::FindPrevious,
            key(command_shift, Code::KeyG),
        );
        let format = item(
            "Format SQL",
            Action::Format,
            key(Modifiers::CONTROL, Code::KeyF),
        );
        let run = item(
            "Run Statement",
            Action::RunStatement,
            key(command, Code::Enter),
        );
        let run_all = item("Run All", Action::RunAll, key(command_shift, Code::Enter));
        let cancel = item("Cancel", Action::Cancel, key(command, Code::Period));
        let history = item("Query History", Action::QueryHistory, None);
        let explain = item("Explain", Action::Explain, key(command, Code::KeyE));
        let explain_analyze = item("Explain Analyze", Action::ExplainAnalyze, None);
        let activity = item("Activity", Action::Activity, None);
        let roles = item("Roles", Action::Roles, None);
        let diagnostics = item("Diagnostics", Action::Diagnostics, None);
        let refresh = item("Refresh", Action::Refresh, key(command, Code::KeyR));
        let add_row = item("Add Row", Action::AddRow, None);
        let delete_rows = item("Delete Selected Rows", Action::DeleteRows, None);
        let discard = item("Discard Changes", Action::DiscardChanges, None);
        let filter = item("Filter", Action::ToggleFilter, None);
        let details = item("Row Details", Action::ToggleDetails, None);
        let back = item("Back", Action::Back, key(command, Code::BracketLeft));
        let servers = item("Servers", Action::Servers, None);
        let disconnect = item("Disconnect", Action::Disconnect, None);
        let system = item(
            "System Appearance",
            Action::Appearance(Appearance::System),
            None,
        );
        let light = item("Light", Action::Appearance(Appearance::Light), None);
        let dark = item("Dark", Action::Appearance(Appearance::Dark), None);

        let app = Submenu::with_items(
            "Postern",
            true,
            &[
                &PredefinedMenuItem::about(
                    None,
                    Some(AboutMetadata {
                        name: Some("Postern".into()),
                        version: Some(env!("CARGO_PKG_VERSION").into()),
                        comments: Some(env!("CARGO_PKG_DESCRIPTION").into()),
                        copyright: Some("Copyright © 2026 Lucky-Dutch. MIT License.".into()),
                        ..Default::default()
                    }),
                ),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ],
        )
        .ok()?;
        let file = Submenu::with_items(
            "File",
            true,
            &[
                &new_query,
                &open,
                &PredefinedMenuItem::separator(),
                &save,
                &save_as,
                &PredefinedMenuItem::separator(),
                &close,
            ],
        )
        .ok()?;
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &find,
                &find_next,
                &find_previous,
                &PredefinedMenuItem::separator(),
                &format,
            ],
        )
        .ok()?;
        let query = Submenu::with_items(
            "Query",
            true,
            &[
                &run,
                &run_all,
                &cancel,
                &PredefinedMenuItem::separator(),
                &explain,
                &explain_analyze,
                &PredefinedMenuItem::separator(),
                &history,
            ],
        )
        .ok()?;
        let table = Submenu::with_items(
            "Table",
            true,
            &[
                &refresh,
                &PredefinedMenuItem::separator(),
                &add_row,
                &delete_rows,
                &discard,
                &PredefinedMenuItem::separator(),
                &filter,
                &details,
                &back,
            ],
        )
        .ok()?;
        let appearance = Submenu::with_items("Appearance", true, &[&system, &light, &dark]).ok()?;
        let view = Submenu::with_items(
            "View",
            true,
            &[
                &servers,
                &activity,
                &roles,
                &diagnostics,
                &disconnect,
                &PredefinedMenuItem::separator(),
                &appearance,
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::fullscreen(None),
            ],
        )
        .ok()?;
        let window = Submenu::with_items(
            "Window",
            true,
            &[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::maximize(None),
            ],
        )
        .ok()?;
        let menu = Menu::with_items(&[&app, &file, &edit, &query, &table, &view, &window]).ok()?;
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        let (sender, events) = mpsc::channel();
        let context = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = sender.send(event);
            context.request_repaint();
        }));
        Some(Self {
            _menu: menu,
            actions,
            events,
        })
    }

    /// Actions chosen from the menu since the last frame.
    pub(crate) fn poll(&self) -> Vec<Action> {
        self.events
            .try_iter()
            .filter_map(|event| self.actions.get(event.id()).copied())
            .collect()
    }
}
