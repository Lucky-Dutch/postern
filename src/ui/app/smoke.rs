use super::*;

const UNICODE_SAMPLES: &[&str] = &[
    "抹茶ラーメン",
    "牛奶饮料",
    "우유 라면",
    "حليب",
    "दूध",
    "นม",
    "Яшкино вафли",
    "Zażółć gęślą jaźń",
];

#[cfg(target_os = "macos")]
const CLIPBOARD_SQL: &str = "SELECT 'Zażółć gęślą jaźń', '抹茶';\n";

#[cfg(target_os = "macos")]
fn assert_clipboard(expected: &str) {
    let output = std::process::Command::new("/usr/bin/pbpaste")
        .output()
        .expect("Read the macOS clipboard");
    assert!(output.status.success(), "pbpaste failed");
    // Do not print clipboard contents when the native backend is broken.
    assert!(
        output.stdout == expected.as_bytes(),
        "The system clipboard did not receive the copied text"
    );
}

pub(super) struct Smoke {
    screen: String,
    phase: u8,
    started: Instant,
    ready: Option<Instant>,
}

impl PosternApp {
    pub(super) fn init_smoke(&mut self) {
        let Ok(screen) = std::env::var("POSTERN_SMOKE_SCREEN") else {
            return;
        };
        let profile = ConnectionProfile {
            name: "Local development".into(),
            host: "127.0.0.1".into(),
            port: std::env::var("POSTERN_TEST_PORT").unwrap().parse().unwrap(),
            username: "postern_test".into(),
            database: "postgres".into(),
            ssl: SslMode::Disable,
            ..Default::default()
        };
        self.profiles = vec![
            profile.clone(),
            ConnectionProfile {
                name: "Analytics".into(),
                database: "analytics".into(),
                color: 1,
                ..profile.clone()
            },
        ];
        self.profiles[1].id = Uuid::new_v4();
        self.draft = profile.clone();
        if screen.ends_with("_light") {
            self.ui_state.appearance = Appearance::Light;
        }
        self.smoke = Some(Smoke {
            screen: screen.clone(),
            phase: if screen == "servers" { 2 } else { 0 },
            started: Instant::now(),
            ready: None,
        });
        if screen == "discovery" {
            self.draft.database.clear();
            self.send(
                Command::DiscoverDatabases(self.draft.clone(), String::new()),
                "Looking up databases...",
            );
        } else if screen != "servers" {
            self.send(Command::Connect(profile, String::new()), "Connecting...");
        }
    }

    pub(super) fn tick_smoke(&mut self, ctx: &egui::Context) {
        let Some(mut smoke) = self.smoke.take() else {
            return;
        };
        assert!(self.error.is_none(), "Smoke test failed: {:?}", self.error);
        #[cfg(target_os = "macos")]
        assert!(self.menu.is_some(), "The native menu bar must be installed");
        assert!(
            smoke.started.elapsed().as_secs() < 30,
            "Smoke test timed out"
        );
        ctx.request_repaint_after(Duration::from_millis(50));
        if std::env::var_os("POSTERN_SMOKE_DEBUG").is_some() {
            eprintln!(
                "phase={} busy={} status={:?} warning={:?} rows={}",
                smoke.phase,
                self.busy.any(),
                self.status,
                self.warning,
                self.table_page.result.rows.len()
            );
        }
        if smoke.phase == 0 && smoke.screen == "discovery" && !self.busy.any() {
            assert!(
                self.discovered_databases
                    .as_ref()
                    .is_some_and(|names| names.contains(&"analytics".into()))
            );
            assert!(self.connected.is_none());
            smoke.phase = 2;
        } else if smoke.phase == 0 && self.connected.is_some() && !self.busy.any() {
            #[cfg(target_os = "macos")]
            if smoke.screen == "clipboard" {
                // The same output path used by the Slow queries Copy button.
                ctx.copy_text("SELECT $1::text;\n".into());
                let prefix = "-- clipboard\n";
                self.open_in_editor("Clipboard".into(), format!("{prefix}{CLIPBOARD_SQL}"));
                let id = self.documents[self.document_index].id;
                let start = prefix.chars().count();
                self.pending_select = Some((id, start..start + CLIPBOARD_SQL.chars().count()));
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique(("sql_editor", id))));
                smoke.phase = 14;
                self.smoke = Some(smoke);
                return;
            }
            if smoke.screen == "routine" {
                let routine = self
                    .routines
                    .iter()
                    .find(|r| r.name == "product_count")
                    .expect("Missing smoke function")
                    .clone();
                self.open_routine(routine);
                smoke.phase = 8;
                self.smoke = Some(smoke);
                return;
            }
            if matches!(
                smoke.screen.as_str(),
                "activity"
                    | "roles"
                    | "explain"
                    | "advisor"
                    | "slow_queries"
                    | "generic_plan"
                    | "statements_setup"
                    | "statements_restart"
            ) {
                match smoke.screen.as_str() {
                    "activity" => self.open_activity(),
                    "roles" => self.open_roles(),
                    "advisor" => {
                        self.diagnostics_tab = super::diagnostics::DiagnosticsTab::Indexes;
                        self.open_diagnostics();
                    }
                    "slow_queries" | "generic_plan" | "statements_setup" | "statements_restart" => {
                        self.diagnostics_tab = super::diagnostics::DiagnosticsTab::Queries;
                        self.open_diagnostics();
                    }
                    _ => {
                        self.mode = Mode::Query;
                        self.documents[0].sql = "SELECT o.id, c.name, sum(i.quantity) AS items\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nJOIN order_items i ON i.order_id = o.id\nGROUP BY o.id, c.name\nORDER BY o.id;".into();
                        self.editor_cursor = 5;
                        self.run_explain(true);
                    }
                }
                smoke.phase = 10;
                self.smoke = Some(smoke);
                return;
            }
            if matches!(smoke.screen.as_str(), "completion" | "find") {
                self.mode = Mode::Query;
                let document = &mut self.documents[0];
                document.sql = "SELECT p.\nFROM products AS p\nWHERE p.category = 'Coffee'\nORDER BY p.price DESC;\n\nSELECT category, count(*)\nFROM products\nGROUP BY category;".into();
                document.folder = "Reports".into();
                let id = document.id;
                self.open_in_editor("Scratch".into(), "SELECT 1;".into());
                self.select_document(0);
                if smoke.screen == "completion" {
                    self.pending_cursor = Some((id, 9));
                } else {
                    self.open_find();
                    if let Some(find) = &mut self.find {
                        find.query = "products".into();
                    }
                }
                smoke.phase = 9;
                smoke.ready = Some(Instant::now());
                self.smoke = Some(smoke);
                return;
            }
            if smoke.screen == "unicode_query" {
                self.documents[0].sql = "-- 抹茶 / 牛奶 / 우유 / حليب / दूध / นม\nSELECT id, language, value\nFROM public.unicode_samples\nORDER BY id;".into();
                self.run_query(RunScope::All);
            } else if smoke.screen == "query" || smoke.screen == "query_light" {
                self.documents[0].sql = "SELECT id, name, category, price, in_stock\nFROM public.products\nWHERE price > 20\nORDER BY price DESC;\n\nSELECT category, count(*) AS products\nFROM public.products\nGROUP BY category;".into();
                self.run_query(RunScope::All);
            } else {
                let relation = self
                    .relations
                    .iter()
                    .find(|r| {
                        r.name
                            == if smoke.screen.starts_with("unicode") {
                                "unicode_samples"
                            } else if smoke.screen == "related" {
                                "orders"
                            } else {
                                "products"
                            }
                    })
                    .expect("Missing smoke fixture")
                    .clone();
                self.select_relation(relation);
            }
            smoke.phase = 1;
        } else if smoke.phase == 1 && !self.busy.any() {
            if smoke.screen.starts_with("unicode") {
                let result = if smoke.screen == "unicode_query" {
                    &self.query_outputs[&self.documents[0].id].results[0]
                } else {
                    &self.table_page.result
                };
                let values: Vec<_> = result
                    .rows
                    .iter()
                    .map(|row| row[2].as_deref().unwrap())
                    .collect();
                assert_eq!(
                    values, UNICODE_SAMPLES,
                    "Unicode values must remain unchanged"
                );
                ctx.fonts_mut(|fonts| {
                    for font in [
                        egui::FontId::proportional(14.0),
                        egui::FontId::monospace(14.0),
                    ] {
                        for sample in UNICODE_SAMPLES {
                            for character in sample.chars().filter(|c| !c.is_whitespace()) {
                                assert!(
                                    fonts.has_glyph(&font, character),
                                    "Missing glyph {character:?} in {:?}",
                                    font.family
                                );
                            }
                        }
                    }
                });
            }
            if smoke.screen == "databases" {
                self.content.selection.click(1, false);
                self.documents[0].sql =
                    "SELECT 'keep this draft after switching databases';".into();
                self.send(
                    Command::SwitchDatabase("analytics".into()),
                    "Switching database...",
                );
                smoke.phase = 4;
                self.smoke = Some(smoke);
                return;
            }
            if matches!(smoke.screen.as_str(), "selection" | "delete") {
                self.content.selection.click(1, false);
                self.content.selection.click(5, true);
                if smoke.screen == "delete" {
                    self.stage_delete();
                }
            }
            if smoke.screen == "structure" {
                self.mode = Mode::Structure;
            }
            if smoke.screen == "ddl" {
                self.mode = Mode::Definition;
                assert!(
                    self.structure
                        .definition
                        .contains("GENERATED ALWAYS AS IDENTITY")
                );
            }
            if smoke.screen == "structure_edit" {
                self.mode = Mode::Structure;
                let relation = self.selected_relation.clone().unwrap();
                let original = TableDraft::from_structure(&relation, &self.structure);
                let mut edited = original.clone();
                edited.columns[1].name = "title".into();
                edited.columns.push(ColumnDraft {
                    name: "weight_grams".into(),
                    data_type: "integer".into(),
                    nullable: true,
                    comment: "Shipping weight".into(),
                    ..Default::default()
                });
                let statements = sql::alter_table_sql(&original, &edited).unwrap();
                assert_eq!(statements.len(), 3);
                self.table_editor = Some(TableEditor {
                    original,
                    edited,
                    creating: false,
                });
            }
            if smoke.screen == "staged" {
                let rows = self.table_page.result.rows.clone();
                let loaded = rows.len();
                self.edits
                    .set(&rows, 0, 1, Some("Ceramic pour-over (v2)".into()));
                self.edits.set(&rows, 1, 3, Some("19.00".into()));
                let index = self.edits.add_row(self.structure.columns.len());
                for (column, value) in [(1, "Travel mug"), (2, "Coffee"), (3, "27.00")] {
                    self.edits
                        .set(&rows, loaded + index, column, Some(value.into()));
                }
                self.edits.delete(loaded, [3]);
                assert_eq!(self.edits.len(), 4);
                self.show_details = true;
                self.content.selection.click(0, false);
                self.content.cell = Some((0, 1));
            }
            if smoke.screen == "filter" {
                let filter = FilterSet {
                    conditions: vec![
                        Filter {
                            column: "category".into(),
                            operator: FilterOperator::Equal,
                            value: "Coffee".into(),
                        },
                        Filter {
                            column: "price".into(),
                            operator: FilterOperator::Greater,
                            value: "30".into(),
                        },
                    ],
                    ..Default::default()
                };
                self.filter = filter.clone();
                self.show_filter = true;
                self.apply_filter(filter);
                smoke.phase = 6;
                self.smoke = Some(smoke);
                return;
            }
            if smoke.screen == "related" {
                let customers = self
                    .relations
                    .iter()
                    .find(|r| r.name == "customers")
                    .unwrap()
                    .clone();
                let customer = self.table_page.result.rows[1][1].clone().unwrap();
                self.open_related(
                    customers,
                    FilterSet::single(Filter {
                        column: "id".into(),
                        operator: FilterOperator::Equal,
                        value: customer,
                    }),
                );
                smoke.phase = 7;
                self.smoke = Some(smoke);
                return;
            }
            if matches!(smoke.screen.as_str(), "edit" | "unicode_edit") {
                let column = if smoke.screen == "unicode_edit" { 2 } else { 1 };
                let values = self.table_page.result.rows[0].clone();
                self.dialog = Some(Dialog::Cell {
                    row: 0,
                    column,
                    name: self.table_page.result.columns[column].clone(),
                    value: values[column].clone().unwrap(),
                    null: false,
                    editable: true,
                    pretty: false,
                });
            }
            smoke.phase = 2;
        } else if smoke.phase == 4 && !self.busy.any() {
            assert_eq!(self.connected.as_ref().unwrap().database, "analytics");
            assert!(self.content.selection.is_empty());
            assert!(self.selected_relation.is_none());
            assert!(self.query_outputs.is_empty());
            assert_eq!(
                self.documents[0].sql,
                "SELECT 'keep this draft after switching databases';"
            );
            let relation = self
                .relations
                .iter()
                .find(|r| r.name == "events")
                .unwrap()
                .clone();
            self.select_relation(relation);
            smoke.phase = 5;
        } else if smoke.phase == 5 && !self.busy.any() {
            assert_eq!(self.table_page.result.rows.len(), 3);
            smoke.phase = 2;
        } else if smoke.phase == 6 && !self.busy.any() {
            assert_eq!(
                self.table_page.result.rows.len(),
                3,
                "Coffee items above 30"
            );
            smoke.phase = 2;
        } else if smoke.phase == 8 && !self.busy.any() {
            assert!(
                self.routine_definition
                    .as_deref()
                    .is_some_and(|d| d.contains("product_count"))
            );
            assert!(self.relations.iter().any(|r| {
                r.name == "measurements_2025"
                    && r.parent
                        .as_ref()
                        .is_some_and(|(_, parent)| parent == "measurements")
            }));
            smoke.phase = 2;
        } else if smoke.phase == 9
            && smoke
                .ready
                .is_some_and(|ready| ready.elapsed() > Duration::from_millis(300))
            // The editor applies the cursor when it draws; the first frames can
            // take longer than the fixed delay.
            && self.pending_cursor.is_none()
        {
            if smoke.screen == "completion" {
                assert_eq!(self.editor_cursor, 9);
                self.complete_at_cursor();
                assert!(
                    self.completion.is_some(),
                    "completion for p. must list products columns: {} relations, {} tables with columns, products columns {:?}, document {:?}",
                    self.completion_catalog.relations.len(),
                    self.completion_catalog.columns.len(),
                    self.completion_catalog
                        .columns
                        .get(&("public".to_owned(), "products".to_owned())),
                    self.documents[self.document_index].sql
                );
            } else {
                self.open_find();
            }
            smoke.ready = None;
            smoke.phase = 2;
        } else if smoke.phase == 10 && !self.busy.any() {
            match smoke.screen.as_str() {
                "activity" => assert!(self.activity.iter().any(|s| s.own)),
                "roles" => assert!(self.roles.iter().any(|r| r.name == "postern_test")),
                "advisor" => {
                    let report = self.index_report.as_ref().expect("index report");
                    for (kind, object) in [
                        (AdviceKind::DuplicateIndex, "products_name_copy"),
                        (AdviceKind::RedundantIndex, "products_category"),
                    ] {
                        assert!(
                            report
                                .items
                                .iter()
                                .any(|i| i.kind == kind && i.object == object),
                            "missing {kind:?} {object}"
                        );
                    }
                    assert!(report.items.iter().any(|i| i.kind
                        == AdviceKind::ForeignKeyWithoutIndex
                        && i.table == "orders"));
                }
                "slow_queries" => {
                    let report = self.slow_queries.as_ref().expect("slow queries");
                    assert_eq!(report.status, StatementsStatus::Ready);
                    assert!(
                        report
                            .queries
                            .iter()
                            .any(|q| q.query.contains("page_views"))
                    );
                }
                "statements_setup" | "statements_restart" => {
                    let report = self.slow_queries.as_ref().expect("slow queries");
                    let StatementsStatus::Setup(setup) = &report.status else {
                        panic!("the setup server must not load pg_stat_statements yet");
                    };
                    let RestartSupport::Available {
                        data_directory,
                        log_file,
                        other_sessions,
                    } = setup.restart.clone()
                    else {
                        panic!(
                            "the local setup server must be restartable: {:?}",
                            setup.restart
                        );
                    };
                    if smoke.screen == "statements_setup" {
                        self.dialog = Some(Dialog::SetUpQueryStats {
                            data_directory,
                            log_file,
                            other_sessions,
                            configure: !setup.configured,
                        });
                    } else {
                        // What the confirmation's Restart server button sends.
                        self.send(
                            Command::SetUpQueryStats(self.slow_request),
                            "Setting up pg_stat_statements...",
                        );
                        smoke.phase = 13;
                        self.smoke = Some(smoke);
                        return;
                    }
                }
                "generic_plan" => {
                    let report = self.slow_queries.as_ref().expect("slow queries");
                    assert!(report.generic_plans, "generic plans need PostgreSQL 16");
                    let query = report
                        .queries
                        .iter()
                        .find(|q| q.query.contains("WHERE path = $1"))
                        .expect("recorded page_views lookup")
                        .query
                        .clone();
                    self.explain_recorded("Slow query".into(), format!("{query};\n"));
                    smoke.phase = 11;
                    self.smoke = Some(smoke);
                    return;
                }
                _ => assert!(
                    self.plans
                        .get(&self.documents[0].id)
                        .is_some_and(|plan| plan.analyzed),
                    "EXPLAIN ANALYZE must produce a measured plan"
                ),
            }
            smoke.phase = 2;
        } else if smoke.phase == 13 && !self.busy.any() {
            let report = self.slow_queries.as_ref().expect("slow queries");
            assert_eq!(
                report.status,
                StatementsStatus::Ready,
                "setup failed: {:?}",
                self.error
            );
            assert!(
                self.warning
                    .as_deref()
                    .is_some_and(|warning| warning.contains("restarted")),
                "{:?}",
                self.warning
            );
            smoke.phase = 2;
        } else if smoke.phase == 11 && !self.busy.any() {
            let id = self.documents[self.document_index].id;
            assert!(
                self.plans.get(&id).is_some_and(|plan| !plan.analyzed),
                "a recorded statement must get a generic plan: {:?}",
                self.error
            );
            smoke.phase = 2;
        } else if smoke.phase == 7 && !self.busy.any() {
            assert_eq!(self.selected_relation.as_ref().unwrap().name, "customers");
            assert_eq!(self.table_page.result.rows.len(), 1);
            assert_eq!(self.back_stack.len(), 1);
            smoke.phase = 2;
        }
        #[cfg(target_os = "macos")]
        match smoke.phase {
            14 if self.pending_select.is_none() => {
                assert_clipboard("SELECT $1::text;\n");
                // egui-winit translates Command-C into this event.
                ctx.input_mut(|input| input.events.push(egui::Event::Copy));
                smoke.phase = 15;
            }
            15 => {
                assert_clipboard(CLIPBOARD_SQL);
                self.documents[self.document_index].sql.clear();
                let id = self.documents[self.document_index].id;
                self.pending_select = Some((id, 0..0));
                smoke.phase = 16;
            }
            16 if self.pending_select.is_none() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                smoke.phase = 17;
            }
            17 if self.documents[self.document_index].sql == CLIPBOARD_SQL => {
                println!("System clipboard copy, editor selection, and paste passed");
                smoke.phase = 2;
            }
            _ => {}
        }
        if smoke.phase == 2 {
            let ready = smoke.ready.get_or_insert_with(Instant::now);
            if ready.elapsed() > Duration::from_millis(400) {
                smoke.phase = 3;
                // Staged changes would otherwise cancel the window close.
                self.allow_close = true;
                let path = std::env::var("POSTERN_CAPTURE").expect("Set POSTERN_CAPTURE");
                let context = ctx.clone();
                ctx.request_screenshot(move |image| {
                    let file = std::fs::File::create(&path).unwrap();
                    let mut encoder = png::Encoder::new(
                        std::io::BufWriter::new(file),
                        image.width() as u32,
                        image.height() as u32,
                    );
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder
                        .write_header()
                        .unwrap()
                        .write_image_data(image.as_raw())
                        .unwrap();
                    println!("Captured {path}");
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                });
            }
        }
        self.smoke = Some(smoke);
    }

    pub(super) fn open_smoke_combo(smoke: Option<&Smoke>, ui: &egui::Ui, id: &str, screen: &str) {
        if smoke.is_some_and(|smoke| smoke.screen == screen && smoke.phase == 2) {
            egui::Popup::open_id(
                ui.ctx(),
                ui.make_persistent_id(egui::IdSalt::new(id)).with("popup"),
            );
        }
    }
}
