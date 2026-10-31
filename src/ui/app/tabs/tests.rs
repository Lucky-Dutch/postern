use super::*;

fn app() -> PosternApp {
    let mut app = PosternApp::from_context(
        &egui::Context::default(),
        Err(anyhow::anyhow!("No persistent storage in this test")),
    )
    .unwrap();
    app.error = None;
    app.servers_visible = false;
    app
}

fn relation(schema: &str, name: &str) -> Relation {
    Relation {
        schema: schema.into(),
        name: name.into(),
        kind: "r".into(),
        ..Default::default()
    }
}

fn page(value: &str) -> TablePage {
    TablePage {
        result: QueryResult {
            columns: vec!["value".into()],
            rows: vec![vec![Some(value.into())]],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn opening_tables_preserves_browse_state_and_staged_changes() {
    let mut app = app();
    let first = relation("public", "items");
    app.select_relation(first.clone());
    let tab = &mut app.table_tabs[0];
    tab.table_page = page("original");
    tab.edits
        .set(&tab.table_page.result.rows, 0, 0, Some("edited".into()));
    tab.filter = FilterSet::single(Filter {
        column: "value".into(),
        value: "draft".into(),
        ..Default::default()
    });
    tab.applied_filter = FilterSet::single(Filter {
        column: "value".into(),
        value: "applied".into(),
        ..Default::default()
    });
    tab.sort = vec![SortColumn {
        name: "value".into(),
        descending: true,
    }];
    tab.nulls = NullsOrder::First;
    tab.page = 3;
    tab.page_size = 25;
    tab.row_count = Some(100);
    tab.show_filter = true;
    tab.show_details = true;
    tab.content.selection.click(0, false);
    tab.content.cell = Some((0, 0));
    tab.back_stack.push(BrowseRequest::default());
    tab.mode = Mode::Structure;

    app.select_relation(relation("archive", "items"));
    assert_eq!(app.table_tabs.len(), 2, "Schema names distinguish tables");
    assert!(app.table_tabs[1].edits.is_empty());
    app.select_relation(first);
    assert_eq!(
        app.table_tabs.len(),
        2,
        "Opening an existing table reuses its tab"
    );
    assert_eq!(app.table_index, 0);
    assert!(app.mode == Mode::Structure);
    let tab = &app.table_tabs[0];
    assert_eq!(tab.table_page.result.rows, page("original").result.rows);
    assert_eq!(
        tab.edits.value(&tab.table_page.result.rows, 0, 0),
        Some("edited")
    );
    assert_eq!(tab.filter.conditions[0].value, "draft");
    assert_eq!(tab.applied_filter.conditions[0].value, "applied");
    assert!(tab.sort[0].descending);
    assert_eq!(tab.nulls, NullsOrder::First);
    assert_eq!((tab.page, tab.page_size, tab.row_count), (3, 25, Some(100)));
    assert!(tab.show_filter && tab.show_details && tab.content.selection.contains(0));
    assert_eq!(tab.content.cell, Some((0, 0)));
    assert_eq!(tab.back_stack.len(), 1);
}

#[test]
fn background_load_page_count_and_error_stay_with_the_requesting_tab() {
    let mut app = app();
    let first = relation("public", "first");
    app.select_relation(first.clone());
    let owner = app.table_tabs[0].id;
    app.busy.data = true;
    app.data_tab = Some(owner);
    app.select_relation(relation("public", "second"));
    assert_eq!(app.table_index, 1, "Switching is allowed during a load");
    app.handle_event(Lane::Data, Event::Loaded(Box::default(), page("first")));
    assert_eq!(app.table_index, 1);
    assert_eq!(
        app.table_tabs[0].table_page.result.rows,
        page("first").result.rows
    );
    assert!(app.table_tabs[1].table_page.result.rows.is_empty());

    app.table_tabs[0].pending_page = 4;
    app.handle_event(Lane::Data, Event::Page(page("next")));
    assert_eq!(app.table_tabs[0].page, 4);
    assert_eq!(app.table_tabs[1].page, 0);
    app.count_tab = Some(owner);
    app.handle_event(
        Lane::Sql(COUNT_SESSION),
        Event::Counted(first.clone(), FilterSet::default(), 500),
    );
    assert_eq!(app.table_tabs[0].row_count, Some(500));
    assert_eq!(app.table_tabs[1].row_count, None);
    app.table_tabs[0].applied_filter.custom_sql = "false".into();
    app.handle_event(
        Lane::Sql(COUNT_SESSION),
        Event::Counted(first, FilterSet::default(), 600),
    );
    assert_eq!(
        app.table_tabs[0].row_count,
        Some(500),
        "A stale filter count is ignored"
    );

    app.table_tabs[0].loading_rows = true;
    app.table_tabs[1].table_page = page("second");
    app.handle_event(Lane::Data, Event::Error("Load failed".into()));
    assert_eq!(app.table_index, 1);
    assert!(!app.table_tabs[0].loading_rows);
    assert!(app.table_tabs[0].table_page.result.rows.is_empty());
    assert_eq!(
        app.table_tabs[1].table_page.result.rows,
        page("second").result.rows
    );
}

#[test]
fn saving_an_inactive_tab_does_not_clear_another_tabs_edits() {
    let mut app = app();
    app.select_relation(relation("public", "first"));
    app.table_tabs[0].edits.add_row(1);
    let owner = app.table_tabs[0].id;
    app.busy.data = true;
    app.data_tab = Some(owner);
    app.select_relation(relation("public", "second"));
    app.table_tabs[1].edits.add_row(2);
    app.handle_event(Lane::Data, Event::Mutated);
    assert_eq!(app.table_index, 1);
    assert!(app.table_tabs[0].edits.is_empty());
    assert_eq!(app.table_tabs[1].edits.inserts.len(), 1);
    assert_eq!(
        app.data_tab,
        Some(owner),
        "The refresh still belongs to the saved table"
    );
}

#[test]
fn closing_dirty_tabs_requires_confirmation_and_keeps_the_active_tab() {
    let mut app = app();
    app.select_relation(relation("public", "first"));
    let first = app.table_tabs[0].id;
    app.table_tabs[0].edits.add_row(1);
    app.select_relation(relation("public", "second"));
    let second = app.table_tabs[1].id;
    app.request_close_table(first);
    assert!(matches!(app.dialog, Some(Dialog::CloseTable(id)) if id == first));
    assert_eq!(app.table_tabs.len(), 2);
    app.dialog = None;
    app.close_table(first);
    assert_eq!(app.table_tabs[app.table_index].id, second);
    app.perform(Action::CloseQuery);
    assert!(app.mode == Mode::Query);
    assert_eq!(app.table_tabs.len(), 1);
    assert!(!app.table_tabs[0].is_open());
}

#[test]
fn running_operations_block_only_their_own_tab_from_closing() {
    let mut app = app();
    app.select_relation(relation("public", "first"));
    let first = app.table_tabs[0].id;
    app.busy.data = true;
    app.data_tab = Some(first);
    app.select_relation(relation("public", "second"));
    let second = app.table_tabs[1].id;
    app.request_close_table(first);
    assert_eq!(app.table_tabs.len(), 2);
    app.request_close_table(second);
    assert_eq!(app.table_tabs.len(), 1);
    assert_eq!(app.table_tabs[0].id, first);
}

#[test]
fn late_counts_from_closed_tabs_cannot_update_reopened_tables() {
    let mut app = app();
    let table = relation("public", "items");
    app.select_relation(table.clone());
    let old = app.table_tabs[0].id;
    app.count_tab = Some(old);
    app.close_table(old);
    app.select_relation(table.clone());
    app.handle_event(
        Lane::Sql(COUNT_SESSION),
        Event::Counted(table, FilterSet::default(), 123),
    );
    assert_ne!(app.table_tabs[0].id, old);
    assert_eq!(app.table_tabs[0].row_count, None);
}

#[test]
fn reconnect_disconnect_and_database_switch_protect_inactive_edits() {
    let mut app = app();
    app.select_relation(relation("public", "first"));
    app.table_tabs[0].edits.add_row(1);
    app.select_relation(relation("public", "second"));
    for command in [
        Command::Disconnect,
        Command::SwitchDatabase("other".into()),
        Command::Connect(ConnectionProfile::default(), String::new()),
    ] {
        assert!(!app.dispatch(command));
        assert!(!app.busy.control);
        assert!(app.warning.as_ref().unwrap().contains("public.first"));
    }
    app.table_tabs[0].edits.clear();
    app.handle_event(Lane::Control, Event::Disconnected);
    assert!(!app.table_tabs[0].is_open());
    app.select_relation(relation("public", "third"));
    app.handle_event(
        Lane::Control,
        Event::Connected(ConnectionProfile::default(), vec![]),
    );
    assert_eq!(app.table_tabs.len(), 1);
    assert!(!app.table_tabs[0].is_open());
    assert!(app.mode == Mode::Query);
}

#[test]
fn structure_drafts_are_independent_and_ddl_refresh_preserves_visible_tab() {
    let mut app = app();
    app.start_new_table("public".into());
    let owner = app.table_tabs[0].id;
    app.table_tabs[0].table_editor.as_mut().unwrap().edited.name = "created".into();
    app.select_relation(relation("public", "other"));
    let visible = app.table_tabs[1].id;
    assert!(app.blocked_by_any_edits());
    assert!(
        !app.blocked_by_any_row_edits(),
        "A structure draft can be applied"
    );
    app.connected = Some(ConnectionProfile::default());
    app.ddl_tab = Some(owner);
    app.reopen_after_catalog = Some(("public".into(), "created".into()));
    app.handle_event(Lane::Data, Event::Executed("Created table".into()));
    assert!(app.table_tabs[0].table_editor.is_none());
    app.handle_event(
        Lane::Data,
        Event::Catalog(vec![
            relation("public", "created"),
            relation("public", "other"),
        ]),
    );
    assert_eq!(app.table_tabs[app.table_index].id, visible);
    assert_eq!(
        app.table_tabs[0].selected_relation.as_ref().unwrap().name,
        "created"
    );
    assert!(app.table_tabs[0].mode == Mode::Structure);
    assert_eq!(app.data_tab, Some(owner));
}

#[test]
fn catalog_refresh_removes_missing_clean_tabs_and_preserves_dirty_ones() {
    let mut app = app();
    app.select_relation(relation("public", "dirty"));
    app.table_tabs[0].edits.add_row(1);
    app.select_relation(relation("public", "dropped"));
    app.select_relation(relation("public", "kept"));
    let visible = app.table_tabs[2].id;
    app.handle_event(Lane::Data, Event::Catalog(vec![relation("public", "kept")]));
    assert_eq!(app.table_tabs.len(), 2);
    assert!(app.table_tabs[0].has_changes());
    assert_eq!(app.table_tabs[app.table_index].id, visible);
}

#[test]
fn cancelling_a_new_table_returns_to_the_neighboring_tab() {
    let mut app = app();
    app.select_relation(relation("public", "items"));
    let table = app.table_tabs[0].id;
    app.start_new_table("public".into());
    app.cancel_table_editor();
    assert_eq!(app.table_tabs.len(), 1);
    assert_eq!(app.table_tabs[app.table_index].id, table);
    assert!(app.mode == Mode::Content);
}

#[test]
fn a_structure_save_in_the_background_keeps_the_sql_editor_visible() {
    let mut app = app();
    app.start_new_table("public".into());
    let table = app.table_tabs[0].id;
    app.ddl_tab = Some(table);
    app.reopen_after_catalog = Some(("public".into(), "created".into()));
    app.select_document(0);
    app.table_tabs[0].table_editor = None;
    app.handle_event(
        Lane::Data,
        Event::Catalog(vec![relation("public", "created")]),
    );
    assert!(app.mode == Mode::Query);
    assert_eq!(app.table_tabs[0].id, table);
    assert_eq!(
        app.table_tabs[0].selected_relation.as_ref().unwrap().name,
        "created"
    );
}
