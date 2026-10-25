//! The object list in the sidebar: tables, views, and partitions grouped by
//! schema; functions and procedures; and their context actions.

use super::*;
use egui_extras::syntax_highlighting::code_view_ui;

/// What an object list item asked for.
enum ObjectRequest {
    Open(Relation),
    Action(Relation, RelationAction),
    RunNow(Relation, RelationAction),
    Select(Relation),
    NewTable(String),
    OpenRoutine(Routine),
    DropRoutine(Routine),
    RoutineInEditor(Routine),
}

impl PosternApp {
    pub(super) fn object_list(&mut self, ui: &mut egui::Ui) {
        let data_busy = self.busy.blocks(Lane::Data);
        let mut request = None;
        ui.horizontal(|ui| {
            ui.strong("Tables & views");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!data_busy, |ui| {
                    ui.menu_button("+", |ui| {
                        if ui.button("New table...").clicked() {
                            let schema = self
                                .selected_relation
                                .as_ref()
                                .map_or_else(|| "public".to_owned(), |r| r.schema.clone());
                            request = Some(ObjectRequest::NewTable(schema));
                            ui.close();
                        }
                        if ui.button("New schema...").clicked() {
                            self.dialog = Some(Dialog::CreateSchema(String::new()));
                            ui.close();
                        }
                    });
                    if ui.small_button("Refresh").clicked() {
                        self.send(Command::Catalog, "Refreshing objects...");
                    }
                });
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.relation_search)
                .hint_text("Search tables and functions")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        let search = self.relation_search.to_lowercase();
        let matches = |schema: &str, name: &str| {
            search.is_empty() || format!("{schema}.{name}").to_lowercase().contains(&search)
        };
        egui::ScrollArea::vertical()
            .id_salt("relations")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_enabled_ui(!data_busy, |ui| {
                    let mut schema = "";
                    for relation in &self.relations {
                        let nested = search.is_empty()
                            && relation.parent.as_ref().is_some_and(|(s, n)| {
                                self.relations
                                    .iter()
                                    .any(|r| &r.schema == s && &r.name == n)
                            });
                        if nested || !matches(&relation.schema, &relation.name) {
                            continue;
                        }
                        if schema != relation.schema {
                            schema = &relation.schema;
                            ui.add_space(9.0);
                            ui.label(
                                RichText::new(schema.to_uppercase())
                                    .small()
                                    .color(theme::muted()),
                            );
                        }
                        let partitions: Vec<&Relation> = self
                            .relations
                            .iter()
                            .filter(|r| {
                                r.parent.as_ref().is_some_and(|(s, n)| {
                                    s == &relation.schema && n == &relation.name
                                })
                            })
                            .collect();
                        if partitions.is_empty() || !search.is_empty() {
                            self.relation_item(ui, relation, &mut request);
                        } else {
                            let id =
                                ui.make_persistent_id(("partitions", relation.qualified_name()));
                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                id,
                                false,
                            )
                            .show_header(ui, |ui| self.relation_item(ui, relation, &mut request))
                            .body(|ui| {
                                ui.label(
                                    RichText::new(format!("{} partitions", partitions.len()))
                                        .small()
                                        .color(theme::muted()),
                                );
                                for partition in partitions {
                                    self.relation_item(ui, partition, &mut request);
                                }
                            });
                        }
                    }
                    if self.relations.is_empty() {
                        ui.label(RichText::new("No user tables or views.").color(theme::muted()));
                    }
                    let routines: Vec<&Routine> = self
                        .routines
                        .iter()
                        .filter(|r| matches(&r.schema, &r.name))
                        .collect();
                    if !routines.is_empty() {
                        ui.add_space(12.0);
                        egui::CollapsingHeader::new(
                            RichText::new(format!("Functions ({})", routines.len())).strong(),
                        )
                        .id_salt("routines")
                        .default_open(!search.is_empty())
                        .show(ui, |ui| {
                            let mut schema = "";
                            for routine in routines {
                                if schema != routine.schema {
                                    schema = &routine.schema;
                                    ui.label(
                                        RichText::new(schema.to_uppercase())
                                            .small()
                                            .color(theme::muted()),
                                    );
                                }
                                let chosen = self.mode == Mode::Routine
                                    && self
                                        .selected_routine
                                        .as_ref()
                                        .is_some_and(|r| r.oid == routine.oid);
                                let label = format!("{}({})", routine.name, routine.arguments);
                                let response =
                                    theme::navigation_item(ui, &label, chosen, "function")
                                        .on_hover_text(format!(
                                            "{} {}\n{}",
                                            if routine.procedure {
                                                "Procedure"
                                            } else {
                                                "Function"
                                            },
                                            routine.signature(),
                                            if routine.result.is_empty() {
                                                String::new()
                                            } else {
                                                format!("Returns {}", routine.result)
                                            }
                                        ));
                                if response.clicked() {
                                    request = Some(ObjectRequest::OpenRoutine(routine.clone()));
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Copy signature").clicked() {
                                        ui.ctx().copy_text(routine.signature());
                                        ui.close();
                                    }
                                    if ui.button("Open definition in SQL editor").clicked() {
                                        request =
                                            Some(ObjectRequest::RoutineInEditor(routine.clone()));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button("Drop...").clicked() {
                                        request = Some(ObjectRequest::DropRoutine(routine.clone()));
                                        ui.close();
                                    }
                                });
                            }
                        });
                    }
                });
            });
        match request {
            Some(ObjectRequest::Open(relation)) => self.select_relation(relation),
            Some(ObjectRequest::Action(relation, action)) => {
                self.dialog = Some(Dialog::RelationAction { relation, action });
            }
            Some(ObjectRequest::RunNow(relation, action)) => {
                match sql::relation_action_sql(&relation, &action) {
                    Ok(statement) => {
                        let description = format!("{statement} completed");
                        self.run_ddl(vec![statement], false, &description, None);
                    }
                    Err(error) => self.warning = Some(error.to_string()),
                }
            }
            Some(ObjectRequest::Select(relation)) => {
                let sql = format!("SELECT *\nFROM {}\nLIMIT 100;", relation.qualified_name());
                self.open_in_editor(format!("{}.sql", relation.name), sql);
            }
            Some(ObjectRequest::NewTable(schema)) => self.start_new_table(schema),
            Some(ObjectRequest::OpenRoutine(routine)) => self.open_routine(routine),
            Some(ObjectRequest::DropRoutine(routine)) => {
                self.dialog = Some(Dialog::DropRoutine {
                    routine,
                    cascade: false,
                });
            }
            Some(ObjectRequest::RoutineInEditor(routine)) => {
                self.open_routine(routine);
                self.open_routine_in_editor = true;
            }
            None => {}
        }
    }

    fn relation_item(
        &self,
        ui: &mut egui::Ui,
        relation: &Relation,
        request: &mut Option<ObjectRequest>,
    ) {
        let chosen = self.selected_relation.as_ref().is_some_and(|selected| {
            selected.schema == relation.schema && selected.name == relation.name
        }) && matches!(
            self.mode,
            Mode::Content | Mode::Structure | Mode::Definition
        );
        let kind = match relation.kind.as_str() {
            "r" | "p" => "table",
            "m" => "materialized",
            _ => "view",
        };
        let response =
            theme::navigation_item(ui, &relation.name, chosen, kind).on_hover_text(format!(
                "{}.{} ({})\nApproximately {} rows",
                relation.schema,
                relation.name,
                relation.kind_label(),
                relation.estimated_rows
            ));
        if response.clicked() {
            *request = Some(ObjectRequest::Open(relation.clone()));
        }
        response.context_menu(|ui| {
            if ui.button("Copy qualified name").clicked() {
                ui.ctx().copy_text(relation.qualified_name());
                ui.close();
            }
            if ui.button("Open SELECT in SQL editor").clicked() {
                *request = Some(ObjectRequest::Select(relation.clone()));
                ui.close();
            }
            ui.separator();
            let mut action =
                |ui: &mut egui::Ui, label: &str, value: RelationAction, confirm: bool| {
                    if ui.button(label).clicked() {
                        *request = Some(if confirm {
                            ObjectRequest::Action(relation.clone(), value)
                        } else {
                            ObjectRequest::RunNow(relation.clone(), value)
                        });
                        ui.close();
                    }
                };
            action(
                ui,
                "Rename...",
                RelationAction::Rename(relation.name.clone()),
                true,
            );
            if relation.is_table() {
                action(
                    ui,
                    "Truncate...",
                    RelationAction::Truncate {
                        cascade: false,
                        restart_identity: false,
                    },
                    true,
                );
            }
            action(ui, "Drop...", RelationAction::Drop { cascade: false }, true);
            if matches!(relation.kind.as_str(), "r" | "p" | "m") {
                ui.separator();
                action(
                    ui,
                    "Vacuum and analyze",
                    RelationAction::VacuumAnalyze,
                    false,
                );
                action(ui, "Analyze", RelationAction::Analyze, false);
                action(ui, "Reindex", RelationAction::Reindex, false);
            }
            if relation.kind == "m" {
                action(
                    ui,
                    "Refresh materialized view",
                    RelationAction::RefreshMaterializedView,
                    false,
                );
            }
        });
    }

    pub(super) fn routine_view(&mut self, ui: &mut egui::Ui) {
        let Some(routine) = self.selected_routine.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Select a function.").color(theme::muted()));
            });
            return;
        };
        if self.open_routine_in_editor
            && let Some(definition) = self.routine_definition.clone()
        {
            self.open_routine_in_editor = false;
            self.open_in_editor(format!("{}.sql", routine.name), definition);
            return;
        }
        egui::Panel::top("routine_bar")
            .frame(egui::Frame::new().fill(theme::background()).inner_margin(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            if routine.procedure { "Procedure" } else { "Function" },
                            routine.signature()
                        ))
                        .color(theme::muted()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let loaded = self.routine_definition.is_some();
                        if ui
                            .add_enabled(!self.busy.blocks(Lane::Data), egui::Button::new("Drop..."))
                            .clicked()
                        {
                            self.dialog = Some(Dialog::DropRoutine {
                                routine: routine.clone(),
                                cascade: false,
                            });
                        }
                        if ui.add_enabled(loaded, egui::Button::new("Copy")).clicked()
                            && let Some(definition) = &self.routine_definition
                        {
                            ui.ctx().copy_text(definition.clone());
                        }
                        if theme::primary(ui, "Edit in SQL editor")
                            .on_hover_text("Opens CREATE OR REPLACE in a new query. Run it to apply your changes.")
                            .clicked()
                            && let Some(definition) = self.routine_definition.clone()
                        {
                            self.open_in_editor(format!("{}.sql", routine.name), definition);
                        }
                    });
                });
            });
        egui::ScrollArea::both().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(16)
                .show(ui, |ui| match &self.routine_definition {
                    Some(definition) => {
                        code_view_ui(ui, &theme::code_theme(14.0), definition, "sql");
                    }
                    None => {
                        ui.label(RichText::new("Loading definition...").color(theme::muted()));
                    }
                });
        });
    }
}
