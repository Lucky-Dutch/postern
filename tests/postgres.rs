use postern::{
    models::*,
    services::database::{Database, MAX_RESULT_ROWS},
    sql::quote_ident,
};

fn profile() -> ConnectionProfile {
    ConnectionProfile {
        host: "127.0.0.1".into(),
        port: std::env::var("POSTERN_TEST_PORT")
            .expect("Set POSTERN_TEST_PORT to a disposable test server")
            .parse()
            .unwrap(),
        username: "postern_test".into(),
        database: "postgres".into(),
        ssl: SslMode::Disable,
        ..Default::default()
    }
}

#[test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
fn database_discovery_and_switching_preserve_permissions_and_active_sessions() -> anyhow::Result<()>
{
    use postern::services::worker::{Command, Event, Worker};
    fn wait(worker: &Worker) -> Event {
        let start = std::time::Instant::now();
        loop {
            if let Some(event) = worker.poll() {
                if !event.finishes() {
                    continue;
                }
                return event;
            }
            assert!(start.elapsed().as_secs() < 10, "Worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let admin = runtime.block_on(Database::connect(profile(), ""))?;
    let prefix = format!("discovery_{}", uuid::Uuid::new_v4().simple());
    let available = format!("{prefix} quoted\" name");
    let forbidden = format!("{prefix}_forbidden");
    let offline = format!("{prefix}_offline");
    let role = format!("{prefix}_user");
    for name in [&available, &forbidden, &offline] {
        runtime.block_on(
            admin
                .client
                .batch_execute(&format!("CREATE DATABASE {}", quote_ident(name))),
        )?;
    }
    runtime.block_on(admin.client.batch_execute(&format!(
        "CREATE ROLE {role} LOGIN; REVOKE CONNECT ON DATABASE {} FROM PUBLIC; ALTER DATABASE {} ALLOW_CONNECTIONS false",
        quote_ident(&forbidden), quote_ident(&offline)
    )))?;
    let mut fixture_profile = profile();
    fixture_profile.database = available.clone();
    let fixture = runtime.block_on(Database::connect(fixture_profile, ""))?;
    runtime.block_on(fixture.client.batch_execute(
        "CREATE TABLE public.marker (value text); INSERT INTO public.marker VALUES ('new database'); GRANT SELECT ON public.marker TO PUBLIC"
    ))?;
    drop(fixture);
    let user = ConnectionProfile {
        username: role.clone(),
        ..profile()
    };
    let db = runtime.block_on(Database::connect(user.clone(), ""))?;
    let listed = runtime.block_on(db.databases())?;
    assert!(listed.contains(&available));
    assert!(listed.contains(&"postgres".to_owned()));
    assert!(!listed.contains(&forbidden));
    assert!(!listed.contains(&offline));
    assert!(!listed.contains(&"template0".to_owned()));
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(listed, sorted);
    for database in [
        String::new(),
        format!("{prefix}_missing"),
        forbidden.clone(),
    ] {
        let draft = ConnectionProfile {
            database,
            ..user.clone()
        };
        assert_eq!(
            runtime.block_on(Database::discover_databases(draft, ""))?,
            listed
        );
    }
    drop(db);

    let worker = Worker::start(|| {})?;
    worker.send(Command::Connect(user.clone(), String::new()))?;
    assert!(matches!(wait(&worker), Event::Connected(..)));
    worker.send(Command::Databases)?;
    assert!(matches!(wait(&worker), Event::Databases(databases) if databases == listed));
    worker.send(Command::DiscoverDatabases(
        ConnectionProfile {
            database: String::new(),
            ..user.clone()
        },
        String::new(),
    ))?;
    assert!(
        matches!(wait(&worker), Event::DiscoveredDatabases(_, databases) if databases == listed)
    );
    worker.send(Command::Query(
        "SELECT current_database(), current_user".into(),
    ))?;
    assert!(
        matches!(wait(&worker), Event::Query(output) if output.results[0].rows == vec![vec![Some("postgres".into()), Some(role.clone())]])
    );

    worker.send(Command::Query(
        "BEGIN; CREATE TEMP TABLE pending (value integer); INSERT INTO pending VALUES (7)".into(),
    ))?;
    assert!(matches!(wait(&worker), Event::Query(output) if output.error.is_none()));
    worker.send(Command::SwitchDatabase(available.clone()))?;
    assert!(matches!(wait(&worker), Event::Error(error) if error.contains("COMMIT or ROLLBACK")));
    worker.send(Command::Query("SELECT value FROM pending; ROLLBACK".into()))?;
    assert!(
        matches!(wait(&worker), Event::Query(output) if output.error.is_none() && output.results[0].rows == vec![vec![Some("7".into())]])
    );
    worker.send(Command::Query("BEGIN; SELECT 1 / 0".into()))?;
    assert!(matches!(wait(&worker), Event::Query(output) if output.error.is_some()));
    worker.send(Command::SwitchDatabase(available.clone()))?;
    assert!(matches!(wait(&worker), Event::Error(error) if error.contains("COMMIT or ROLLBACK")));
    worker.send(Command::Query("ROLLBACK".into()))?;
    assert!(matches!(wait(&worker), Event::Query(output) if output.error.is_none()));

    for target in [format!("{prefix}_missing"), forbidden.clone()] {
        worker.send(Command::SwitchDatabase(target))?;
        assert!(matches!(wait(&worker), Event::Error(_)));
        worker.send(Command::Query("SELECT current_database()".into()))?;
        assert!(
            matches!(wait(&worker), Event::Query(output) if output.results[0].rows == vec![vec![Some("postgres".into())]])
        );
    }
    worker.send(Command::SwitchDatabase(available.clone()))?;
    let Event::Connected(connected, relations) = wait(&worker) else {
        panic!("Expected switched database");
    };
    assert_eq!(connected.database, available);
    assert_eq!(connected.username, role);
    let marker = relations
        .into_iter()
        .find(|r| r.name == "marker")
        .expect("Expected the new catalog");
    worker.send(Command::Load(marker, 100))?;
    assert!(
        matches!(wait(&worker), Event::Loaded(_, page) if page.result.rows == vec![vec![Some("new database".into())]])
    );
    worker.send(Command::Query(
        "SELECT current_database(), current_user, value FROM public.marker".into(),
    ))?;
    assert!(
        matches!(wait(&worker), Event::Query(output) if output.error.is_none() && output.results[0].rows == vec![vec![Some(available.clone()), Some(role.clone()), Some("new database".into())]])
    );
    worker.send(Command::SwitchDatabase("postgres".into()))?;
    assert!(
        matches!(wait(&worker), Event::Connected(connected, _) if connected.database == "postgres")
    );
    worker.send(Command::Disconnect)?;
    assert!(matches!(wait(&worker), Event::Disconnected));
    worker.send(Command::SwitchDatabase(available.clone()))?;
    assert!(matches!(wait(&worker), Event::Error(_)));
    drop(worker);
    for name in [&available, &forbidden, &offline] {
        runtime.block_on(
            admin
                .client
                .batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", quote_ident(name))),
        )?;
    }
    runtime.block_on(admin.client.batch_execute(&format!("DROP ROLE {role}")))?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn postgres_roundtrip_and_conflict_protection() -> anyhow::Result<()> {
    let mut db = Database::connect(profile(), "").await?;
    let schema = format!("test_{}", uuid::Uuid::new_v4().simple());
    let quoted = quote_ident(&schema);
    db.client.batch_execute(&format!(
        "CREATE SCHEMA {quoted}; \
         CREATE TYPE {quoted}.mood AS ENUM ('calm', 'busy'); \
         CREATE TABLE {quoted}.items (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, title text NOT NULL DEFAULT 'untitled', amount numeric(30,10), enabled boolean, payload jsonb, raw bytea, labels text[], mood {quoted}.mood, doubled bigint GENERATED ALWAYS AS (id * 2) STORED); \
         INSERT INTO {quoted}.items (title, amount, enabled, payload, raw, labels, mood) VALUES \
         ('a', 12345678901234567890.1234567890, true, '{{\"a\":1}}', decode('00ff', 'hex'), ARRAY['x','y'], 'calm'), \
         ('b', NULL, false, NULL, NULL, NULL, 'busy'), ('c', 3, NULL, NULL, NULL, NULL, NULL); \
         CREATE VIEW {quoted}.item_names AS SELECT title FROM {quoted}.items; \
         CREATE TABLE {quoted}.no_key (value text); INSERT INTO {quoted}.no_key VALUES ('x');"
    )).await?;
    let result = async {
        let relations = db.relations().await?;
        let relation = relations.iter().find(|r| r.schema == schema && r.name == "items").unwrap().clone();
        let structure = db.structure(&relation).await?;
        let target = MutationTarget { relation: relation.clone(), columns: structure.columns.clone() };
        assert_eq!(structure.columns.len(), 9);
        assert!(structure.columns[0].primary_key);
        assert!(structure.columns[8].generated);
        assert!(!structure.constraints.is_empty());
        assert!(!structure.indexes.is_empty());
        let mut request = BrowseRequest { relation: relation.clone(), columns: structure.columns.clone(), filter: FilterSet::default(), sort: vec![], nulls: NullsOrder::Default, page: 0, page_size: 2 };
        let page = db.browse(&request).await?;
        assert_eq!(page.result.rows.len(), 2);
        assert!(page.has_more);
        assert_eq!(page.result.rows[0][2].as_deref(), Some("12345678901234567890.1234567890"));
        assert_eq!(page.result.rows[0][5].as_deref(), Some("\\x00ff"));
        assert_eq!(page.result.rows[1][2], None);
        request.page = 1;
        let next = db.browse(&request).await?;
        assert_eq!(next.result.rows[0][1].as_deref(), Some("c"));
        assert!(!next.has_more);
        request.page = 0;
        request.filter = FilterSet::single(Filter { column: "title".into(), operator: FilterOperator::Equal, value: "' OR true --".into() });
        assert!(db.browse(&request).await?.result.rows.is_empty());
        request.filter = FilterSet::single(Filter { column: "title".into(), operator: FilterOperator::InList, value: "b\nc".into() });
        request.sort = vec![SortColumn { name: "title".into(), descending: true }];
        assert_eq!(db.browse(&request).await?.result.rows[0][1].as_deref(), Some("c"));
        db.update_cell(&target, &page.result.rows[0], 1, Some("a'quoted".into())).await?;
        assert!(db.update_cell(&target, &page.result.rows[0], 1, Some("stale".into())).await.is_err());
        assert!(db.update_cell(&target, &page.result.rows[1], 8, Some("999".into())).await.is_err());
        assert!(db.update_cell(&target, &page.result.rows[1], 1, None).await.is_err());
        db.update_cell(&target, &page.result.rows[1], 2, Some("42.5".into())).await?;
        request.filter = FilterSet::default(); request.sort.clear();
        let fresh = db.browse(&request).await?;
        assert_eq!(fresh.result.rows[0][1].as_deref(), Some("a'quoted"));
        assert_eq!(fresh.result.rows[1][2].as_deref(), Some("42.5000000000"));
        assert!(db.delete_row(&target, &page.result.rows[0]).await.is_err());
        db.delete_row(&target, &fresh.result.rows[0]).await?;
        let mut values = vec![InsertValue::Default; structure.columns.len()];
        values[1] = InsertValue::Text("inserted".into());
        values[7] = InsertValue::Text("calm".into());
        db.insert_row(&target, &values).await?;
        let no_key = relations.iter().find(|r| r.schema == schema && r.name == "no_key").unwrap();
        assert!(db.delete_row(&MutationTarget { relation: no_key.clone(), columns: db.structure(no_key).await?.columns }, &[Some("x".into())]).await.is_err());
        let view = relations.iter().find(|r| r.schema == schema && r.name == "item_names").unwrap();
        assert!(db.delete_row(&MutationTarget { relation: view.clone(), columns: db.structure(view).await?.columns }, &[Some("b".into())]).await.is_err());
        let results = db.query("SELECT 1 AS first WHERE false; SELECT NULL::text AS missing, '' AS empty; SELECT 3 AS third").await?;
        assert!(results.error.is_none());
        assert_eq!(results.results.len(), 3);
        assert_eq!(results.results[0].columns, ["first"]);
        assert!(results.results[0].rows.is_empty());
        assert_eq!(results.results[1].rows[0], [None, Some(String::new())]);
        let results = db.query("SELECT i FROM generate_series(1, 6000) i; SELECT 7;").await?;
        assert_eq!(results.results[0].rows.len(), MAX_RESULT_ROWS);
        assert!(results.results[0].truncated);
        assert!(results.results[1].truncated);
        assert_eq!(results.results[0].affected, 6000);
        let results = db.query("SELECT 1; SELECT 1/0;").await?;
        assert!(results.error.as_deref().unwrap().contains("22012"));
        assert_eq!(db.query("SELECT 2").await?.results[0].rows[0][0].as_deref(), Some("2"));
        let token = db.client.cancel_token();
        let cancellation = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            token.cancel_query(tokio_postgres::NoTls).await.unwrap();
        });
        let start = std::time::Instant::now();
        let output = db.query("SELECT pg_sleep(15)").await?;
        cancellation.await?;
        assert!(output.error.as_deref().unwrap().contains("57014"));
        assert!(start.elapsed().as_secs() < 5);
        assert!(db.query("SELECT 1").await?.error.is_none());
        db.client.batch_execute(&format!("INSERT INTO {quoted}.items (title) SELECT 'extra' FROM generate_series(1, 12)")).await?;
        request.page_size = 100;
        let ordered = db.browse(&request).await?;
        let ids: Vec<i64> = ordered.result.rows.iter().map(|r| r[0].as_ref().unwrap().parse().unwrap()).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "Primary key ordering must stay numeric: {ids:?}");
        request.sort = vec![SortColumn { name: "id".into(), descending: true }];
        let ordered = db.browse(&request).await?;
        let ids: Vec<i64> = ordered.result.rows.iter().map(|r| r[0].as_ref().unwrap().parse().unwrap()).collect();
        assert!(ids.windows(2).all(|pair| pair[0] > pair[1]), "Explicit numeric sorting must not use text output aliases: {ids:?}");
        db.client.batch_execute(&format!("ALTER TABLE {quoted}.items RENAME COLUMN title TO renamed_title")).await?;
        assert!(db.update_cell(&target, &fresh.result.rows[1], 1, Some("wrong target".into())).await.unwrap_err().to_string().contains("structure changed"));
        assert!(db.insert_row(&target, &values).await.is_err());
        anyhow::Ok(())
    }.await;
    db.client
        .batch_execute(&format!("DROP SCHEMA {quoted} CASCADE"))
        .await?;
    result
}

#[test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
fn worker_isolates_transactions_and_recovers_after_cancel() -> anyhow::Result<()> {
    use postern::services::worker::{Command, Event, Worker};
    fn wait(worker: &Worker) -> Event {
        let start = std::time::Instant::now();
        loop {
            if let Some(event) = worker.poll() {
                if let Event::Error(error) = &event {
                    panic!("Worker failed: {error}");
                }
                if !event.finishes() {
                    continue;
                }
                return event;
            }
            assert!(start.elapsed().as_secs() < 10, "Worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let schema = format!("worker_{}", uuid::Uuid::new_v4().simple());
    let db = runtime.block_on(Database::connect(profile(), ""))?;
    runtime.block_on(db.client.batch_execute(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.items (id integer PRIMARY KEY, value text); INSERT INTO {schema}.items VALUES (1, 'original');")))?;
    let worker = Worker::start(|| {})?;
    worker.send(Command::Connect(profile(), String::new()))?;
    let Event::Connected(_, relations) = wait(&worker) else {
        panic!("Expected connection");
    };
    let relation = relations
        .into_iter()
        .find(|r| r.schema == schema && r.name == "items")
        .unwrap();
    worker.send(Command::Query(format!(
        "BEGIN; INSERT INTO {schema}.items VALUES (2, 'uncommitted');"
    )))?;
    assert!(matches!(wait(&worker), Event::Query(output) if output.error.is_none()));
    worker.send(Command::Load(relation.clone(), 100))?;
    let Event::Loaded(structure, page) = wait(&worker) else {
        panic!("Expected loaded table");
    };
    assert_eq!(
        page.result.rows.len(),
        1,
        "Uncommitted SQL data must be invisible to the browser"
    );
    worker.send(Command::Update(
        MutationTarget {
            relation,
            columns: structure.columns,
        },
        page.result.rows[0].clone(),
        1,
        Some("grid edit".into()),
    ))?;
    assert!(matches!(wait(&worker), Event::Mutated));
    worker.send(Command::Query(format!(
        "ROLLBACK; SELECT value FROM {schema}.items ORDER BY id"
    )))?;
    let Event::Query(output) = wait(&worker) else {
        panic!("Expected SQL results");
    };
    assert!(output.error.is_none());
    assert_eq!(output.results[1].rows, vec![vec![Some("grid edit".into())]]);
    worker.send(Command::Query("SELECT pg_sleep(15)".into()))?;
    std::thread::sleep(std::time::Duration::from_millis(150));
    worker.cancel();
    assert!(
        matches!(wait(&worker), Event::Query(output) if output.error.as_deref().unwrap_or_default().contains("57014"))
    );
    worker.send(Command::Query("SELECT 1".into()))?;
    assert!(matches!(wait(&worker), Event::Query(output) if output.error.is_none()));
    worker.send(Command::Catalog)?;
    let Event::Catalog(relations) = wait(&worker) else {
        panic!("Expected catalog");
    };
    let relation = relations
        .into_iter()
        .find(|r| r.schema == schema && r.name == "items")
        .unwrap();
    worker.send(Command::Load(relation.clone(), 100))?;
    let Event::Loaded(structure, page) = wait(&worker) else {
        panic!("Expected loaded table");
    };
    worker.send(Command::Delete(
        MutationTarget {
            relation,
            columns: structure.columns,
        },
        page.result.rows,
    ))?;
    assert!(matches!(wait(&worker), Event::Mutated));
    worker.send(Command::Query(format!("SELECT * FROM {schema}.items")))?;
    assert!(
        matches!(wait(&worker), Event::Query(output) if output.error.is_none() && output.results[0].rows.is_empty())
    );
    worker.send(Command::Disconnect)?;
    assert!(matches!(wait(&worker), Event::Disconnected));
    runtime.block_on(
        db.client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE")),
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn batch_delete_is_atomic_and_respects_composite_keys_and_constraints() -> anyhow::Result<()>
{
    let mut db = Database::connect(profile(), "").await?;
    let schema = format!("batch_{}", uuid::Uuid::new_v4().simple());
    db.client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema};
         CREATE TABLE {schema}.items (
           tenant integer, id integer, value text, parent_id integer,
           PRIMARY KEY (tenant, id),
           FOREIGN KEY (tenant, parent_id) REFERENCES {schema}.items (tenant, id));
         INSERT INTO {schema}.items VALUES
           (1, 1, NULL, NULL), (1, 2, 'two', NULL), (1, 3, 'three', NULL),
           (1, 4, 'child', 1), (2, 1, 'other tenant', NULL);
         CREATE TABLE {schema}.links (tenant integer, item_id integer,
           FOREIGN KEY (tenant, item_id) REFERENCES {schema}.items (tenant, id));
         INSERT INTO {schema}.links VALUES (1, 3);"
        ))
        .await?;
    let result = async {
        let relation = db.relations().await?.into_iter().find(|r| r.schema == schema && r.name == "items").unwrap();
        let columns = db.structure(&relation).await?.columns;
        let target = MutationTarget { relation: relation.clone(), columns: columns.clone() };
        let request = BrowseRequest { relation, columns, filter: FilterSet::default(), sort: vec![], nulls: NullsOrder::Default, page: 0, page_size: 100 };
        let rows = db.browse(&request).await?.result.rows;
        assert!(db.delete_rows(&target, &[]).await.is_err());
        assert!(db.delete_rows(&target, &[rows[1].clone(), rows[1].clone()]).await.is_err());
        assert!(db.delete_rows(&target, &[rows[1].clone(), vec![]]).await.is_err());
        assert_eq!(db.browse(&request).await?.result.rows, rows);

        db.client.batch_execute(&format!("UPDATE {schema}.items SET value = 'changed' WHERE tenant = 1 AND id = 2")).await?;
        let fresh = db.browse(&request).await?.result.rows;
        let error = db.delete_rows(&target, &[rows[0].clone(), rows[3].clone(), rows[1].clone()]).await.unwrap_err();
        assert!(error.to_string().contains("No change saved"));
        assert_eq!(db.browse(&request).await?.result.rows, fresh, "A stale row must roll back every selected deletion");

        assert!(db.delete_rows(&target, &[fresh[1].clone(), fresh[2].clone()]).await.is_err());
        assert_eq!(db.browse(&request).await?.result.rows, fresh, "A foreign key error must roll back the batch");

        db.delete_rows(&target, &[fresh[0].clone(), fresh[3].clone()]).await?;
        let remaining = db.browse(&request).await?.result.rows;
        assert_eq!(remaining, vec![fresh[1].clone(), fresh[2].clone(), fresh[4].clone()], "Delete only the selected composite keys, including a parent and child in the same statement");

        db.client.batch_execute(&format!("ALTER TABLE {schema}.items RENAME COLUMN value TO renamed")).await?;
        assert!(db.delete_rows(&target, &remaining).await.unwrap_err().to_string().contains("Table structure changed"));
        let count: i64 = db.client.query_one(&format!("SELECT count(*) FROM {schema}.items"), &[]).await?.get(0);
        assert_eq!(count, 3);
        Ok::<(), anyhow::Error>(())
    }.await;
    db.client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await?;
    result
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn edits_enforce_type_modifiers_use_the_key_index_and_scripts_run_per_statement()
-> anyhow::Result<()> {
    let mut db = Database::connect(profile(), "").await?;
    let schema = format!("fixes_{}", uuid::Uuid::new_v4().simple());
    db.client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema};
             CREATE TABLE {schema}.codes (id integer PRIMARY KEY, code varchar(3), flag char(2), bits bit(3), doc json, amount numeric(8,2));
             INSERT INTO {schema}.codes SELECT i, 'c' || (i % 100), 'x', B'101', json_build_object('i', i), i FROM generate_series(1, 20000) i;
             ANALYZE {schema}.codes;"
        ))
        .await?;
    let result = async {
        let relation = db
            .relations()
            .await?
            .into_iter()
            .find(|r| r.schema == schema && r.name == "codes")
            .unwrap();
        let structure = db.structure(&relation).await?;
        let target = MutationTarget {
            relation: relation.clone(),
            columns: structure.columns.clone(),
        };
        assert!(!structure.columns[4].sortable, "json has no ordering");
        assert!(structure.columns[1].sortable);
        assert!(
            structure
                .constraints
                .iter()
                .all(|(_, definition)| !definition.starts_with("NOT NULL"))
        );
        let mut request = BrowseRequest {
            relation: relation.clone(),
            columns: structure.columns.clone(),
            page_size: 3,
            ..Default::default()
        };
        let rows = db.browse(&request).await?.result.rows;
        // Over-long values are rejected instead of silently truncated.
        let error = db
            .update_cell(&target, &rows[0], 1, Some("abcdef".into()))
            .await
            .unwrap_err();
        let code = error
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(|error| error.code())
            .map(|code| code.code().to_owned());
        assert_eq!(code.as_deref(), Some("22001"), "{error:?}");
        assert!(
            db.update_cell(&target, &rows[0], 2, Some("toolong".into()))
                .await
                .is_err()
        );
        assert!(
            db.update_cell(&target, &rows[0], 3, Some("1".into()))
                .await
                .is_err()
        );
        let mut values = vec![InsertValue::Default; 6];
        values[0] = InsertValue::Text("30000".into());
        values[1] = InsertValue::Text("abcd".into());
        assert!(db.insert_row(&target, &values).await.is_err());
        let stored: Option<String> = db
            .client
            .query_one(
                &format!("SELECT code FROM {schema}.codes WHERE id = 1"),
                &[],
            )
            .await?
            .get(0);
        assert_eq!(stored.as_deref(), Some("c1"));
        db.update_cell(&target, &rows[0], 1, Some("abc".into()))
            .await?;
        db.update_cell(
            &target,
            &rows[0..1]
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    r[1] = Some("abc".into());
                    r
                })
                .next()
                .unwrap(),
            5,
            Some("12.345".into()),
        )
        .await?;
        let amount: String = db
            .client
            .query_one(
                &format!("SELECT amount::text FROM {schema}.codes WHERE id = 1"),
                &[],
            )
            .await?
            .get(0);
        assert_eq!(amount, "12.35");
        // The row predicate uses the primary key index.
        let mutation = postern::sql::update_row_sql(&target, &rows[1], &[(1, Some("zz".into()))])?;
        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = mutation
            .params
            .iter()
            .map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let plan: Vec<String> = db
            .client
            .query(&format!("EXPLAIN {}", mutation.sql), &params)
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert!(
            plan.iter()
                .any(|line| line.contains("Index Scan") || line.contains("Index Only Scan")),
            "{plan:#?}"
        );
        // Unsortable types sort by text instead of failing; typed filters compare numerically.
        request.sort = vec![SortColumn {
            name: "doc".into(),
            descending: false,
        }];
        assert_eq!(db.browse(&request).await?.result.rows.len(), 3);
        request.sort.clear();
        request.filter = FilterSet::single(Filter {
            column: "amount".into(),
            operator: FilterOperator::Greater,
            value: "19998".into(),
        });
        assert_eq!(db.count(&request).await?, 2);
        request.filter.conditions.push(Filter {
            column: "code".into(),
            operator: FilterOperator::StartsWith,
            value: "C9".into(),
        });
        assert_eq!(db.count(&request).await?, 1);
        request.filter.any = true;
        // (amount > 19998 OR code begins with C9) AND id > 19990: ids 19991-19999 by
        // code and 20000 by amount.
        request.filter.custom_sql = "id > 19990".into();
        assert_eq!(db.count(&request).await?, 10);
        // Scripts run statement by statement: earlier statements commit, the error is located.
        let script = format!(
            "UPDATE {schema}.codes SET code = 'ok' WHERE id = 2;\n\nSELECT 1 / 0;\nSELECT 3;"
        );
        let output = db.query(&script).await?;
        assert_eq!((output.statements, output.completed), (3, 1));
        let error = output.error.unwrap();
        assert!(
            error.contains("22012") && error.contains("Statement 2 of 3"),
            "{error}"
        );
        // Runtime errors have no position, so the failing statement is located.
        let offset = output.error_offset.unwrap();
        assert!(script[offset..].starts_with("SELECT 1 / 0"));
        // Analysis errors point at the offending token, across statements and lines.
        let script = "SELECT 1;\n  SELECT nope FROM pg_class;";
        let output = db.query(script).await?;
        let offset = output.error_offset.unwrap();
        assert!(script[offset..].starts_with("nope"), "{offset}");
        let code: String = db
            .client
            .query_one(
                &format!("SELECT code FROM {schema}.codes WHERE id = 2"),
                &[],
            )
            .await?
            .get(0);
        assert_eq!(code, "ok");
        let output = db.query("VACUUM pg_catalog.pg_class; SELECT 1").await?;
        assert!(output.error.is_none(), "{:?}", output.error);
        let output = db
            .query("DO $$ BEGIN RAISE NOTICE 'hello %', 42; END $$; SELECT 2")
            .await?;
        assert!(
            output.messages.iter().any(|m| m.contains("hello 42")),
            "{:?}",
            output.messages
        );
        let output = db
            .query(&format!(
                "SELECT 1; COPY {schema}.codes FROM STDIN; SELECT 2"
            ))
            .await?;
        assert_eq!(output.completed, 1);
        assert!(output.error.unwrap().contains("not supported"));
        assert_eq!(
            db.query("SELECT 'still usable'").await?.results[0].rows[0][0].as_deref(),
            Some("still usable")
        );
        assert!(db.query("-- nothing to run\n").await?.results.is_empty());
        anyhow::Ok(())
    }
    .await;
    db.client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await?;
    result
}

#[test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
fn worker_lanes_run_concurrently_track_transactions_and_recover_lost_sessions() -> anyhow::Result<()>
{
    use postern::services::worker::{Command, Event, Lane, Worker};
    fn finish(worker: &Worker) -> (Vec<String>, Lane, Event) {
        let start = std::time::Instant::now();
        let mut notices = Vec::new();
        loop {
            if let Some((lane, event)) = worker.poll_lane() {
                if let Event::Notice(notice) = event {
                    notices.push(notice);
                    continue;
                }
                if !event.finishes() {
                    continue;
                }
                return (notices, lane, event);
            }
            assert!(start.elapsed().as_secs() < 15, "Worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let admin = runtime.block_on(Database::connect(profile(), ""))?;
    let schema = format!("lanes_{}", uuid::Uuid::new_v4().simple());
    runtime.block_on(admin.client.batch_execute(&format!(
        "CREATE SCHEMA {schema}; CREATE TABLE {schema}.items (id integer PRIMARY KEY, value text); INSERT INTO {schema}.items VALUES (1, 'a'), (2, 'b'), (3, 'c');"
    )))?;
    let worker = Worker::start(|| {})?;
    let configured = ConnectionProfile {
        startup_sql: format!("SET search_path = {schema}, public"),
        statement_timeout_secs: 0,
        ..profile()
    };
    worker.send(Command::Connect(configured.clone(), String::new()))?;
    let (_, _, Event::Connected(_, relations)) = finish(&worker) else {
        panic!("Expected connection")
    };
    let relation = relations
        .into_iter()
        .find(|r| r.schema == schema && r.name == "items")
        .unwrap();
    // A slow query does not block the table browser.
    worker.send(Command::Query(
        "SELECT pg_sleep(1.5), current_setting('statement_timeout')".into(),
    ))?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    worker.send(Command::Load(relation.clone(), 100))?;
    let (_, lane, Event::Loaded(structure, page)) = finish(&worker) else {
        panic!("Expected table first")
    };
    assert_eq!((lane, page.result.rows.len()), (Lane::Data, 3));
    let (_, lane, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert_eq!(lane, Lane::Sql(postern::services::worker::SHARED_SESSION));
    assert_eq!(
        output.results[0].rows[0][1].as_deref(),
        Some("0"),
        "SQL session has no timeout"
    );
    // Refresh keeps the filter and sort while they still apply.
    let request = BrowseRequest {
        relation: relation.clone(),
        columns: structure.columns.clone(),
        filter: FilterSet::single(Filter {
            column: "value".into(),
            operator: FilterOperator::NotEqual,
            value: "b".into(),
        }),
        sort: vec![SortColumn {
            name: "id".into(),
            descending: true,
        }],
        page_size: 100,
        ..Default::default()
    };
    worker.send(Command::Reload(request))?;
    let (_, _, Event::Reloaded(_, page, kept)) = finish(&worker) else {
        panic!("Expected reload")
    };
    assert_eq!(
        page.result
            .rows
            .iter()
            .map(|r| r[0].clone().unwrap())
            .collect::<Vec<_>>(),
        ["3", "1"]
    );
    assert_eq!((kept.filter.conditions.len(), kept.sort.len()), (1, 1));
    // Startup SQL and the transaction indicator.
    worker.send(Command::Query("SELECT count(*) FROM items; BEGIN".into()))?;
    let (_, _, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert_eq!(output.results[0].rows[0][0].as_deref(), Some("3"));
    assert_eq!(output.transaction, TransactionState::Open);
    worker.send(Command::Query("SELECT 1 / 0".into()))?;
    let (_, _, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert_eq!(output.transaction, TransactionState::Failed);
    worker.send(Command::Query("ROLLBACK".into()))?;
    let (_, _, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert_eq!(output.transaction, TransactionState::Idle);
    // A terminated SQL session is re-established and reported.
    runtime.block_on(admin.client.batch_execute(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = 'Postern SQL'",
    ))?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    worker.send(Command::Query("SELECT 'after loss'".into()))?;
    let (notices, _, event) = finish(&worker);
    match event {
        Event::Query(output) => {
            assert!(
                notices.iter().any(|n| n.contains("re-established")),
                "{notices:?}"
            );
            assert_eq!(output.results[0].rows[0][0].as_deref(), Some("after loss"));
        }
        Event::Error(error) => assert!(error.contains("re-established"), "{error}"),
        _ => panic!("Unexpected event"),
    }
    worker.send(Command::Query(
        "SELECT current_setting('search_path')".into(),
    ))?;
    let (_, _, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert!(
        output.results[0].rows[0][0]
            .as_deref()
            .unwrap()
            .starts_with(&schema),
        "startup SQL runs after reconnecting"
    );
    worker.send(Command::Disconnect)?;
    assert!(matches!(finish(&worker).2, Event::Disconnected));
    // Read-only profiles reject writes in both sessions.
    let read_only = ConnectionProfile {
        read_only: true,
        ..profile()
    };
    worker.send(Command::Connect(read_only, String::new()))?;
    assert!(matches!(finish(&worker).2, Event::Connected(..)));
    worker.send(Command::Query(format!(
        "INSERT INTO {schema}.items VALUES (9, 'x')"
    )))?;
    let (_, _, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert!(output.error.unwrap().contains("25006"));
    worker.send(Command::Insert(
        MutationTarget {
            relation,
            columns: structure.columns,
        },
        vec![InsertValue::Text("9".into()), InsertValue::Text("x".into())],
    ))?;
    let (_, _, Event::Error(error)) = finish(&worker) else {
        panic!("Expected read-only error")
    };
    assert!(error.contains("25006"), "{error}");
    worker.send(Command::Disconnect)?;
    assert!(matches!(finish(&worker).2, Event::Disconnected));
    drop(worker);
    runtime.block_on(
        admin
            .client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE")),
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn staged_changes_csv_import_export_and_foreign_key_metadata() -> anyhow::Result<()> {
    let mut db = Database::connect(profile(), "").await?;
    let schema = format!("staged_{}", uuid::Uuid::new_v4().simple());
    db.client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema};
             CREATE TABLE {schema}.authors (id integer PRIMARY KEY, name text NOT NULL);
             CREATE TABLE {schema}.books (id serial PRIMARY KEY, author_id integer REFERENCES {schema}.authors (id), title text, price numeric(6,2), note text);
             INSERT INTO {schema}.authors VALUES (1, 'Ann'), (2, 'Bob');
             INSERT INTO {schema}.books (author_id, title, price) VALUES (1, 'First', 10), (2, 'Second', 20), (1, 'Third', 30);"
        ))
        .await?;
    let directory = tempfile::tempdir()?;
    let result = async {
        let relations = db.relations().await?;
        let books = relations.iter().find(|r| r.schema == schema && r.name == "books").unwrap().clone();
        let authors = relations.iter().find(|r| r.schema == schema && r.name == "authors").unwrap().clone();
        let structure = db.structure(&books).await?;
        assert_eq!(structure.foreign_keys.len(), 1);
        assert_eq!(structure.foreign_keys[0].columns, ["author_id"]);
        assert_eq!((structure.foreign_keys[0].referenced_table.as_str(), structure.foreign_keys[0].referenced_columns.as_slice()), ("authors", ["id".to_string()].as_slice()));
        let all = db.lookup_keys(&authors, "id", "").await?;
        assert_eq!(all.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert!(all[1].1.contains("\"name\":\"Bob\""), "{all:?}");
        let found = db.lookup_keys(&authors, "id", "BO").await?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "2");
        assert_eq!(db.lookup_keys(&authors, "id", "%").await?.len(), 0, "search text is literal");
        let author_structure = db.structure(&authors).await?;
        assert_eq!(author_structure.referenced_by.len(), 1);
        assert_eq!(author_structure.referenced_by[0].table, "books");
        // Foreign key navigation uses typed equality; 1.0 matches the integer 1.
        let mut request = BrowseRequest { relation: books.clone(), columns: structure.columns.clone(), page_size: 100, ..Default::default() };
        request.filter = FilterSet::single(Filter { column: "author_id".into(), operator: FilterOperator::Equal, value: "1".into() });
        assert_eq!(db.count(&request).await?, 2);
        request.filter = FilterSet::single(Filter { column: "price".into(), operator: FilterOperator::InList, value: "10.0\n30".into() });
        assert_eq!(db.count(&request).await?, 2);
        request.filter = FilterSet::default();
        let rows = db.browse(&request).await?.result.rows;
        let target = MutationTarget { relation: books.clone(), columns: structure.columns.clone() };
        // Updates, inserts, and deletes are saved together.
        let changes = RowChanges {
            updates: vec![(rows[0].clone(), vec![(2, Some("First edition".into())), (4, Some(String::new()))])],
            inserts: vec![vec![InsertValue::Default, InsertValue::Text("2".into()), InsertValue::Text("Fourth".into()), InsertValue::Null, InsertValue::Default]],
            deletes: vec![rows[2].clone()],
        };
        db.apply_changes(&target, &changes).await?;
        let after = db.browse(&request).await?.result.rows;
        assert_eq!(after.len(), 3);
        assert_eq!(after[0][2].as_deref(), Some("First edition"));
        assert_eq!(after[0][4].as_deref(), Some(""));
        assert_eq!(after[2][2].as_deref(), Some("Fourth"));
        assert_eq!(after[2][3], None);
        // A stale row rolls back every staged change, including valid ones.
        let stale = RowChanges {
            updates: vec![(rows[1].clone(), vec![(2, Some("never".into()))])],
            inserts: vec![vec![InsertValue::Default, InsertValue::Text("1".into()), InsertValue::Text("never".into()), InsertValue::Null, InsertValue::Null]],
            deletes: vec![rows[0].clone()],
        };
        assert!(db.apply_changes(&target, &stale).await.unwrap_err().to_string().contains("No change saved"));
        assert_eq!(db.browse(&request).await?.result.rows, after);
        // CSV export streams all rows; the file round-trips NULL and empty strings.
        let path = directory.path().join("books.csv");
        let exported = db.export_table_csv(&request, &path, &|_| {}).await?;
        assert_eq!(exported, 3);
        let csv = std::fs::read_to_string(&path)?;
        assert!(csv.starts_with("\"id\",\"author_id\",\"title\",\"price\",\"note\"\r\n"));
        assert!(csv.contains(",\"\"\r\n"), "an empty string is a quoted empty field: {csv}");
        db.client.batch_execute(&format!("CREATE TABLE {schema}.copy (LIKE {schema}.books)")).await?;
        let copy = Relation { schema: schema.clone(), name: "copy".into(), kind: "r".into(), ..Default::default() };
        let columns: Vec<String> = structure.columns.iter().map(|c| c.name.clone()).collect();
        assert_eq!(db.import_csv(&copy, &columns, &path, true, ',').await?, 3);
        let compare: i64 = db.client.query_one(&format!(
            "SELECT count(*) FROM {schema}.books b JOIN {schema}.copy c ON b.id = c.id AND b.title IS NOT DISTINCT FROM c.title AND b.price IS NOT DISTINCT FROM c.price AND b.note IS NOT DISTINCT FROM c.note"
        ), &[]).await?.get(0);
        assert_eq!(compare, 3, "NULL and empty strings survive export and import");
        // A failing row rolls back the whole import.
        std::fs::write(directory.path().join("bad.csv"), "id;title\n10;ok\nnot-a-number;bad\n")?;
        assert!(db.import_csv(&copy, &["id".into(), "title".into()], &directory.path().join("bad.csv"), true, ';').await.is_err());
        let count: i64 = db.client.query_one(&format!("SELECT count(*) FROM {schema}.copy"), &[]).await?.get(0);
        assert_eq!(count, 3);
        // Query export re-runs a read-only statement through COPY.
        let path = directory.path().join("query.csv");
        let rows = db.export_query_csv(&format!("SELECT id, title FROM {schema}.books ORDER BY id"), &path, &|_| {}).await?;
        assert_eq!(rows, 3);
        assert!(std::fs::read_to_string(&path)?.starts_with("id,title\n1,First edition\n"));
        assert!(db.export_query_csv(&format!("DELETE FROM {schema}.books"), &path, &|_| {}).await.is_err());
        anyhow::Ok(())
    }
    .await;
    db.client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await?;
    result
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn generated_ddl_recreates_tables_and_structure_edits_apply() -> anyhow::Result<()> {
    let mut db = Database::connect(profile(), "").await?;
    let schema = format!("ddl_{}", uuid::Uuid::new_v4().simple());
    let q = quote_ident(&schema);
    db.client
        .batch_execute(&format!(
            "CREATE SCHEMA {q};
             CREATE FUNCTION {q}.touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.updated := now(); RETURN NEW; END $$;
             CREATE FUNCTION {q}.add(a integer, b integer) RETURNS integer LANGUAGE sql AS 'SELECT a + b';
             CREATE PROCEDURE {q}.noop() LANGUAGE sql AS 'SELECT 1';
             CREATE TABLE {q}.parents (id integer PRIMARY KEY);
             CREATE TABLE {q}.items (
               id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
               serial_no serial,
               label text COLLATE \"C\" NOT NULL DEFAULT 'x',
               price numeric(8,2) CHECK (price >= 0),
               doubled numeric GENERATED ALWAYS AS (price * 2) STORED,
               parent_id integer REFERENCES {q}.parents (id) ON DELETE CASCADE,
               updated timestamptz
             ) WITH (fillfactor = 90);
             CREATE INDEX items_label ON {q}.items (lower(label)) WHERE price > 0;
             CREATE TRIGGER items_touch BEFORE UPDATE ON {q}.items FOR EACH ROW EXECUTE FUNCTION {q}.touch();
             ALTER TABLE {q}.items ENABLE ROW LEVEL SECURITY;
             CREATE POLICY visible ON {q}.items FOR SELECT TO PUBLIC USING (price IS NOT NULL);
             COMMENT ON TABLE {q}.items IS 'Item''s table';
             COMMENT ON COLUMN {q}.items.label IS 'Shown';
             CREATE TABLE {q}.events (at date NOT NULL, kind text) PARTITION BY RANGE (at);
             CREATE TABLE {q}.events_2025 PARTITION OF {q}.events FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
             CREATE MATERIALIZED VIEW {q}.item_totals AS SELECT count(*) AS n FROM {q}.items;"
        ))
        .await?;
    let result = async {
        let relations = db.relations().await?;
        let find = |name: &str| {
            relations
                .iter()
                .find(|r| r.schema == schema && r.name == name)
                .unwrap()
                .clone()
        };
        let items = find("items");
        assert_eq!(
            find("events_2025").parent,
            Some((schema.clone(), "events".into()))
        );
        assert_eq!(find("events").parent, None);
        let routines = db.routines().await?;
        let mine: Vec<_> = routines.iter().filter(|r| r.schema == schema).collect();
        assert_eq!(mine.len(), 3);
        let add = mine.iter().find(|r| r.name == "add").unwrap();
        assert_eq!(add.arguments, "a integer, b integer");
        assert!(mine.iter().any(|r| r.name == "noop" && r.procedure));
        assert!(
            db.routine_definition(add.oid)
                .await?
                .starts_with("CREATE OR REPLACE FUNCTION")
        );
        let before = db.structure(&items).await?;
        let stats = before.stats.clone().unwrap();
        assert!(stats.total_bytes > 0);
        assert!(
            before
                .indexes
                .iter()
                .any(|i| i.name == "items_pkey" && i.constraint)
        );
        assert!(
            before
                .indexes
                .iter()
                .any(|i| i.name == "items_label" && !i.constraint)
        );
        let ddl = before.definition.clone();
        for expected in [
            "GENERATED ALWAYS AS IDENTITY",
            "GENERATED ALWAYS AS ((price * (2)::numeric)) STORED",
            "COLLATE pg_catalog.\"C\"",
            "WITH (fillfactor=90)",
            "CREATE SEQUENCE IF NOT EXISTS",
            "OWNED BY",
            "ENABLE ROW LEVEL SECURITY",
            "CREATE POLICY \"visible\" ON",
            "CREATE TRIGGER items_touch",
            "CREATE INDEX items_label",
            "COMMENT ON TABLE",
            "'Item''s table'",
        ] {
            assert!(ddl.contains(expected), "missing {expected} in\n{ddl}");
        }
        let events_ddl = db.structure(&find("events_2025")).await?.definition;
        assert!(
            events_ddl.contains("PARTITION OF") && events_ddl.contains("FOR VALUES FROM"),
            "{events_ddl}"
        );
        assert!(
            db.structure(&find("events"))
                .await?
                .definition
                .contains("PARTITION BY RANGE (at)")
        );
        let totals_ddl = db.structure(&find("item_totals")).await?.definition;
        assert!(
            totals_ddl.contains("CREATE MATERIALIZED VIEW"),
            "{totals_ddl}"
        );
        // Dropping and running the generated DDL recreates an identical table.
        db.execute(
            &[
                format!("DROP MATERIALIZED VIEW {q}.item_totals"),
                format!("DROP TABLE {q}.items"),
            ],
            true,
        )
        .await?;
        db.execute(&[ddl.clone(), totals_ddl], true).await?;
        let after = db.structure(&items).await?;
        assert_eq!(after.columns, before.columns);
        assert_eq!(after.constraints, before.constraints);
        assert_eq!(after.definition, before.definition);
        // Structure edits run as one transaction.
        let original = TableDraft::from_structure(&items, &after);
        let mut edited = original.clone();
        let label = edited
            .columns
            .iter_mut()
            .find(|c| c.name == "label")
            .unwrap();
        label.name = "title".into();
        label.data_type = "varchar(40)".into();
        label.nullable = true;
        edited.columns.push(ColumnDraft {
            name: "stock".into(),
            data_type: "integer".into(),
            nullable: false,
            default: "0".into(),
            ..Default::default()
        });
        edited.constraints.push(ConstraintDraft {
            name: "stock_ok".into(),
            definition: "CHECK (stock >= 0)".into(),
            ..Default::default()
        });
        edited.indexes.push(IndexDraft {
            name: "items_stock".into(),
            columns: "stock".into(),
            method: "btree".into(),
            ..Default::default()
        });
        edited.comment = String::new();
        db.execute(&postern::sql::alter_table_sql(&original, &edited)?, true)
            .await?;
        let changed = db.structure(&items).await?;
        let title = changed.columns.iter().find(|c| c.name == "title").unwrap();
        assert_eq!(
            (title.data_type.as_str(), title.nullable),
            ("character varying(40)", true)
        );
        assert!(
            changed
                .columns
                .iter()
                .any(|c| c.name == "stock" && c.default.as_deref() == Some("0"))
        );
        assert!(changed.indexes.iter().any(|i| i.name == "items_stock"));
        assert!(changed.comment.is_none());
        // A failing statement rolls back the whole structure change.
        let mut broken = TableDraft::from_structure(&items, &changed);
        broken.columns.retain(|c| c.name != "stock");
        broken.columns.push(ColumnDraft {
            name: "bad".into(),
            data_type: "no_such_type".into(),
            nullable: true,
            ..Default::default()
        });
        let mut original = TableDraft::from_structure(&items, &changed);
        original.columns.retain(|c| c.name != "stock");
        broken.name = "renamed".into();
        assert!(
            db.execute(&postern::sql::alter_table_sql(&original, &broken)?, true)
                .await
                .is_err()
        );
        assert!(
            db.relations()
                .await?
                .iter()
                .any(|r| r.schema == schema && r.name == "items")
        );
        // Creating a table and object actions.
        let created = TableDraft {
            schema: schema.clone(),
            name: "notes".into(),
            columns: vec![
                ColumnDraft {
                    name: "id".into(),
                    data_type: "bigint GENERATED ALWAYS AS IDENTITY".into(),
                    primary_key: true,
                    ..Default::default()
                },
                ColumnDraft {
                    name: "body".into(),
                    data_type: "text".into(),
                    nullable: true,
                    comment: "Body".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        db.execute(&postern::sql::create_table_sql(&created)?, true)
            .await?;
        db.client
            .batch_execute(&format!("INSERT INTO {q}.notes (body) VALUES ('a'), ('b')"))
            .await?;
        let notes = db
            .relations()
            .await?
            .into_iter()
            .find(|r| r.schema == schema && r.name == "notes")
            .unwrap();
        assert!(db.structure(&notes).await?.columns[0].primary_key);
        for action in [
            RelationAction::VacuumAnalyze,
            RelationAction::Analyze,
            RelationAction::Reindex,
            RelationAction::Truncate {
                cascade: false,
                restart_identity: true,
            },
        ] {
            db.execute(
                &[postern::sql::relation_action_sql(&notes, &action)?],
                matches!(action, RelationAction::Truncate { .. }),
            )
            .await?;
        }
        let count: i64 = db
            .client
            .query_one(&format!("SELECT count(*) FROM {q}.notes"), &[])
            .await?
            .get(0);
        assert_eq!(count, 0);
        db.execute(
            &[postern::sql::relation_action_sql(
                &notes,
                &RelationAction::Rename("memos".into()),
            )?],
            true,
        )
        .await?;
        let memos = db
            .relations()
            .await?
            .into_iter()
            .find(|r| r.schema == schema && r.name == "memos")
            .unwrap();
        db.execute(
            &[postern::sql::relation_action_sql(
                &memos,
                &RelationAction::Drop { cascade: false },
            )?],
            true,
        )
        .await?;
        db.execute(
            &[postern::sql::relation_action_sql(
                &find("item_totals"),
                &RelationAction::RefreshMaterializedView,
            )?],
            true,
        )
        .await?;
        db.execute(&[postern::sql::drop_routine_sql(add, false)], true)
            .await?;
        assert_eq!(
            db.routines()
                .await?
                .iter()
                .filter(|r| r.schema == schema)
                .count(),
            2
        );
        anyhow::Ok(())
    }
    .await;
    db.client
        .batch_execute(&format!("DROP SCHEMA {q} CASCADE"))
        .await?;
    result
}

#[test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
fn each_document_has_its_own_sql_session() -> anyhow::Result<()> {
    use postern::services::worker::{Command, Event, Lane, Worker};
    fn finish(worker: &Worker) -> (Lane, Event) {
        let start = std::time::Instant::now();
        loop {
            if let Some((lane, event)) = worker.poll_lane() {
                if event.finishes() {
                    return (lane, event);
                }
                continue;
            }
            assert!(start.elapsed().as_secs() < 15, "Worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    fn first_value(event: Event) -> String {
        let Event::Query(output) = event else {
            panic!("Expected a query result")
        };
        assert!(output.error.is_none(), "{:?}", output.error);
        output.results.last().unwrap().rows[0][0].clone().unwrap()
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let admin = runtime.block_on(Database::connect(profile(), ""))?;
    let database = format!("tabs_{}", uuid::Uuid::new_v4().simple());
    runtime.block_on(
        admin
            .client
            .batch_execute(&format!("CREATE DATABASE {database}")),
    )?;
    let worker = Worker::start(|| {})?;
    worker.send(Command::Connect(profile(), String::new()))?;
    assert!(matches!(finish(&worker).1, Event::Connected(..)));
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    worker.send(Command::QueryIn(a, "SELECT pg_backend_pid()".into()))?;
    let pid_a = first_value(finish(&worker).1);
    worker.send(Command::QueryIn(b, "SELECT pg_backend_pid()".into()))?;
    let pid_b = first_value(finish(&worker).1);
    assert_ne!(pid_a, pid_b, "documents use separate sessions");
    worker.send(Command::QueryIn(
        a,
        "BEGIN; CREATE TEMP TABLE only_a (x int); SELECT 1".into(),
    ))?;
    let (_, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert_eq!(output.transaction, TransactionState::Open);
    worker.send(Command::QueryIn(
        b,
        "SELECT count(*) FROM pg_class WHERE relname = 'only_a'".into(),
    ))?;
    assert_eq!(
        first_value(finish(&worker).1),
        "0",
        "uncommitted objects stay in their document's transaction"
    );
    // Documents run at the same time.
    worker.send(Command::QueryIn(a, "SELECT pg_sleep(1.2)::text".into()))?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    worker.send(Command::QueryIn(b, "SELECT 'b first'".into()))?;
    let (lane, event) = finish(&worker);
    assert_eq!(lane, Lane::Sql(b));
    assert_eq!(first_value(event), "b first");
    assert_eq!(finish(&worker).0, Lane::Sql(a));
    // Any open transaction blocks switching databases.
    worker.send(Command::SwitchDatabase(database.clone()))?;
    assert!(
        matches!(finish(&worker).1, Event::Error(error) if error.contains("COMMIT or ROLLBACK"))
    );
    worker.send(Command::QueryIn(a, "ROLLBACK".into()))?;
    let (_, Event::Query(output)) = finish(&worker) else {
        panic!("Expected query")
    };
    assert_eq!(output.transaction, TransactionState::Idle);
    worker.send(Command::CloseSession(b))?;
    assert!(matches!(finish(&worker), (lane, Event::SessionClosed) if lane == Lane::Sql(b)));
    worker.send(Command::SwitchDatabase(database.clone()))?;
    assert!(
        matches!(finish(&worker).1, Event::Connected(profile, _) if profile.database == database)
    );
    worker.send(Command::QueryIn(a, "SELECT current_database()".into()))?;
    assert_eq!(
        first_value(finish(&worker).1),
        database,
        "document sessions reopen on the new database"
    );
    worker.send(Command::Disconnect)?;
    assert!(matches!(finish(&worker).1, Event::Disconnected));
    worker.send(Command::QueryIn(a, "SELECT 1".into()))?;
    assert!(matches!(finish(&worker).1, Event::Error(error) if error.contains("Not connected")));
    drop(worker);
    runtime.block_on(
        admin
            .client
            .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)")),
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn activity_roles_privileges_and_explain_plans() -> anyhow::Result<()> {
    let db = Database::connect(profile(), "").await?;
    let schema = format!("extras_{}", uuid::Uuid::new_v4().simple());
    db.client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema}; CREATE TABLE {schema}.items (id integer PRIMARY KEY, label text);
             INSERT INTO {schema}.items SELECT i, 'x' || i FROM generate_series(1, 500) i; ANALYZE {schema}.items;
             GRANT SELECT, INSERT ON {schema}.items TO PUBLIC;"
        ))
        .await?;
    let result = async {
        let sessions = db.activity().await?;
        let own: i32 = db.client.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        assert!(sessions.iter().any(|s| s.own && s.pid == own));
        // Cancel a query running in another session.
        let other = Database::connect(profile(), "").await?;
        let pid: i32 = other.client.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        let sleeper = tokio::spawn(async move { other.query("SELECT pg_sleep(10)").await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(db.activity().await?.iter().any(|s| s.pid == pid && s.state == "active"));
        assert!(db.signal_backend(pid, false).await?);
        let output = sleeper.await??;
        assert!(output.error.unwrap().contains("57014"));
        let roles = db.roles().await?;
        assert!(roles.iter().any(|r| r.name == "postern_test" && r.superuser && r.login));
        assert!(!roles.iter().any(|r| r.name.starts_with("pg_")));
        let relation = db.relations().await?.into_iter().find(|r| r.schema == schema).unwrap();
        let structure = db.structure(&relation).await?;
        assert!(structure.privileges.iter().any(|(grantee, privileges)| grantee == "PUBLIC" && privileges == "INSERT, SELECT"), "{:?}", structure.privileges);
        // EXPLAIN output parses into a plan; ANALYZE inside BEGIN/ROLLBACK keeps the data.
        let output = db.query(&format!("EXPLAIN (FORMAT JSON) SELECT * FROM {schema}.items WHERE id = 7")).await?;
        let json = output.results[0].rows[0][0].clone().unwrap();
        assert_eq!(output.results[0].columns, ["QUERY PLAN"]);
        let plan = postern::ui::explain::parse(&json).unwrap();
        assert!(plan.root.title.contains("items"), "{}", plan.root.title);
        assert!(!plan.analyzed);
        let output = db.query(&format!("BEGIN;\nEXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) DELETE FROM {schema}.items;\nROLLBACK")).await?;
        assert!(output.error.is_none(), "{:?}", output.error);
        let plan = postern::ui::explain::parse(output.results[1].rows[0][0].as_deref().unwrap()).unwrap();
        assert!(plan.analyzed && plan.execution_ms.is_some());
        assert!(plan.root.title.starts_with("Delete on items"), "{}", plan.root.title);
        let count: i64 = db.client.query_one(&format!("SELECT count(*) FROM {schema}.items"), &[]).await?.get(0);
        assert_eq!(count, 500);
        anyhow::Ok(())
    }
    .await;
    db.client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await?;
    result
}

#[test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
fn sorting_uses_indexes_casts_one_page_and_counts_in_the_background() -> anyhow::Result<()> {
    use postern::services::worker::{COUNT_SESSION, Command, Event, Lane, Worker};
    let runtime = tokio::runtime::Runtime::new()?;
    let db = runtime.block_on(Database::connect(profile(), ""))?;
    let schema = format!("sorting_{}", uuid::Uuid::new_v4().simple());
    runtime.block_on(db.client.batch_execute(&format!(
        "CREATE SCHEMA {schema};
         CREATE TABLE {schema}.events (id integer PRIMARY KEY, at timestamptz, label text);
         INSERT INTO {schema}.events SELECT i, CASE WHEN i % 10 = 0 THEN NULL ELSE now() - i * interval '1 minute' END, 'event ' || i FROM generate_series(1, 30000) i;
         CREATE INDEX events_at ON {schema}.events (at);
         ANALYZE {schema}.events;"
    )))?;
    let result = (|| -> anyhow::Result<()> {
        let relation = runtime
            .block_on(db.relations())?
            .into_iter()
            .find(|r| r.schema == schema)
            .unwrap();
        let columns = runtime.block_on(db.columns(&relation))?;
        let mut request = BrowseRequest {
            relation: relation.clone(),
            columns,
            sort: vec![SortColumn {
                name: "at".into(),
                descending: true,
            }],
            page_size: 100,
            ..Default::default()
        };
        // Default NULL placement lets the index serve a descending sort.
        let (sql, _) = postern::sql::browse_sql(&request)?;
        let plan: Vec<String> = runtime
            .block_on(db.client.query(&format!("EXPLAIN {sql}"), &[]))?
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert!(
            plan.iter()
                .any(|line| line.contains("Index Scan Backward using events_at")),
            "{plan:#?}"
        );
        assert!(
            !plan.iter().any(|line| line.contains("Sort Method")
                || line.trim_start().starts_with("->  Sort ") && line.contains("events")),
            "{plan:#?}"
        );
        let page = runtime.block_on(db.browse(&request))?;
        assert_eq!(
            page.result.rows[0][1], None,
            "PostgreSQL puts NULLs first when descending"
        );
        request.nulls = NullsOrder::Last;
        assert!(runtime.block_on(db.browse(&request))?.result.rows[0][1].is_some());
        request.nulls = NullsOrder::Default;
        // Pages of the subquery form match one ordered query.
        request.sort = vec![SortColumn {
            name: "label".into(),
            descending: false,
        }];
        request.page = 3;
        let page = runtime.block_on(db.browse(&request))?;
        let expected: Vec<String> = runtime
            .block_on(db.client.query(
                &format!(
                    "SELECT label FROM {schema}.events ORDER BY label, id LIMIT 100 OFFSET 300"
                ),
                &[],
            ))?
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(
            page.result
                .rows
                .iter()
                .map(|r| r[2].clone().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(page.has_more);
        // Counting runs on its own session while the table browser keeps working.
        let worker = Worker::start(|| {})?;
        worker.send(Command::Connect(profile(), String::new()))?;
        let wait = |worker: &Worker| -> (Lane, Event) {
            let start = std::time::Instant::now();
            loop {
                if let Some((lane, event)) = worker.poll_lane()
                    && event.finishes()
                {
                    return (lane, event);
                }
                assert!(start.elapsed().as_secs() < 15, "Worker timed out");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        assert!(matches!(wait(&worker).1, Event::Connected(..)));
        let filtered = BrowseRequest {
            filter: FilterSet::single(Filter {
                column: "at".into(),
                operator: FilterOperator::IsNull,
                value: String::new(),
            }),
            page: 0,
            ..request.clone()
        };
        worker.send(Command::Count(filtered.clone()))?;
        worker.send(Command::Browse(request.clone()))?;
        let mut lanes = Vec::new();
        for _ in 0..2 {
            match wait(&worker) {
                (lane, Event::Counted(counted, filter, count)) => {
                    assert_eq!(
                        (lane, counted.name.as_str(), count),
                        (Lane::Sql(COUNT_SESSION), "events", 3000)
                    );
                    assert_eq!(filter, filtered.filter);
                    lanes.push(lane);
                }
                (lane, Event::Page(page)) => {
                    assert_eq!(page.result.rows.len(), 100);
                    lanes.push(lane);
                }
                _ => panic!("Unexpected event"),
            }
        }
        assert!(lanes.contains(&Lane::Data) && lanes.contains(&Lane::Sql(COUNT_SESSION)));
        worker.send(Command::Disconnect)?;
        assert!(matches!(wait(&worker).1, Event::Disconnected));
        Ok(())
    })();
    runtime.block_on(
        db.client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE")),
    )?;
    result
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL server; use scripts/test-postgres.sh"]
async fn index_advisor_and_slow_queries_find_problems_and_offer_fixes() -> anyhow::Result<()> {
    let admin = Database::connect(profile(), "").await?;
    let name = format!("diagnostics_{}", uuid::Uuid::new_v4().simple());
    admin
        .client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await?;
    let mut target = profile();
    target.database = name.clone();
    let result = async {
        let db = Database::connect(target, "").await?;
        db.client
            .batch_execute(
                "CREATE TABLE parent (id integer PRIMARY KEY, code text UNIQUE);
                 CREATE TABLE child (id integer PRIMARY KEY, parent_id integer REFERENCES parent,
                     other_id integer REFERENCES parent, a text, b text, c integer);
                 CREATE INDEX child_other ON child (other_id);
                 CREATE INDEX child_a ON child (a);
                 CREATE INDEX child_a_copy ON child (a);
                 CREATE INDEX child_ab ON child (a, b);
                 CREATE INDEX child_a_pattern ON child (a text_pattern_ops);
                 CREATE INDEX child_c_partial ON child (c) WHERE c > 0;
                 CREATE INDEX parent_code_copy ON parent (code);
                 CREATE TABLE pair (x integer, y integer, PRIMARY KEY (x, y));
                 CREATE TABLE pair_ref (id integer PRIMARY KEY, x integer, y integer,
                     FOREIGN KEY (x, y) REFERENCES pair, FOREIGN KEY (y, x) REFERENCES pair);
                 CREATE INDEX pair_ref_yx ON pair_ref (y, x);
                 CREATE TABLE pair_partial (id integer PRIMARY KEY, x integer, y integer,
                     CONSTRAINT pair_partial_fk FOREIGN KEY (x, y) REFERENCES pair);
                 CREATE INDEX pair_partial_x ON pair_partial (x);
                 INSERT INTO child (id, a) VALUES (1, 'same'), (2, 'same');
                 CREATE TABLE big (id integer PRIMARY KEY, payload text) WITH (autovacuum_enabled = false);
                 INSERT INTO big SELECT i, 'x' FROM generate_series(1, 30000) i;
                 CREATE TABLE churn (id integer PRIMARY KEY, payload text) WITH (autovacuum_enabled = false);
                 INSERT INTO churn SELECT i, 'x' FROM generate_series(1, 20000) i;",
            )
            .await?;
        db.client
            .batch_execute("DELETE FROM churn WHERE id > 5000")
            .await?;
        // A failed concurrent build leaves an invalid index behind.
        assert!(
            db.client
                .batch_execute("CREATE UNIQUE INDEX CONCURRENTLY child_a_unique ON child (a)")
                .await
                .is_err()
        );
        for _ in 0..3 {
            db.client
                .batch_execute("SELECT count(*) FROM big WHERE payload <> 'pgm_scan'")
                .await?;
        }
        // Table statistics reach the shared counters, possibly in several
        // flushes, shortly after the backend goes idle.
        let started = std::time::Instant::now();
        loop {
            let row = db
                .client
                .query_one(
                    "SELECT (SELECT seq_tup_read FROM pg_stat_user_tables WHERE relname = 'big'), \
                     (SELECT n_dead_tup FROM pg_stat_user_tables WHERE relname = 'churn'), \
                     (SELECT n_live_tup FROM pg_stat_user_tables WHERE relname = 'churn')",
                    &[],
                )
                .await?;
            let (read, dead, live): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
            if (read, dead, live) == (90000, 15000, 5000) {
                break;
            }
            assert!(
                started.elapsed().as_secs() < 20,
                "statistics not flushed: {read} {dead} {live}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let report = db.index_report().await?;
        let find = |kind: AdviceKind, object: &str| {
            report
                .items
                .iter()
                .find(|i| i.kind == kind && i.object == object)
                .unwrap_or_else(|| panic!("missing {kind:?} {object}: {:#?}", report.items))
        };
        let listed = |object: &str| -> Vec<AdviceKind> {
            report
                .items
                .iter()
                .filter(|i| i.object == object)
                .map(|i| i.kind)
                .collect()
        };
        assert_eq!(
            find(AdviceKind::InvalidIndex, "child_a_unique").fix.as_deref(),
            Some("REINDEX INDEX CONCURRENTLY \"public\".\"child_a_unique\"")
        );
        let fk = report
            .items
            .iter()
            .find(|i| i.kind == AdviceKind::ForeignKeyWithoutIndex && i.table == "child")
            .expect("child.parent_id has no index");
        assert!(fk.detail.starts_with("(parent_id) references public.parent"), "{}", fk.detail);
        assert_eq!(
            fk.fix.as_deref(),
            Some("CREATE INDEX CONCURRENTLY ON \"public\".\"child\" (\"parent_id\")")
        );
        let unindexed: Vec<_> = report
            .items
            .iter()
            .filter(|i| i.kind == AdviceKind::ForeignKeyWithoutIndex)
            .map(|i| (i.table.as_str(), i.object.as_str()))
            .collect();
        assert_eq!(unindexed.len(), 2, "{unindexed:?}");
        assert!(unindexed.contains(&("pair_partial", "pair_partial_fk")));
        let duplicate = find(AdviceKind::DuplicateIndex, "child_a_copy");
        assert_eq!(duplicate.detail, "Same columns as child_a.");
        assert_eq!(
            duplicate.fix.as_deref(),
            Some("DROP INDEX CONCURRENTLY \"public\".\"child_a_copy\"")
        );
        assert_eq!(
            find(AdviceKind::DuplicateIndex, "parent_code_copy").detail,
            "Same columns as parent_code_key."
        );
        assert_eq!(
            find(AdviceKind::RedundantIndex, "child_a").detail,
            "child_ab starts with the same columns."
        );
        // Each index is listed once, under its most specific finding.
        assert_eq!(listed("child_a"), [AdviceKind::RedundantIndex]);
        assert_eq!(listed("child_a_copy"), [AdviceKind::DuplicateIndex]);
        assert_eq!(listed("child_ab"), [AdviceKind::UnusedIndex]);
        // A different operator class or a partial index is not a duplicate.
        assert_eq!(listed("child_a_pattern"), [AdviceKind::UnusedIndex]);
        assert_eq!(listed("child_c_partial"), [AdviceKind::UnusedIndex]);
        // Keys, unique constraints, and indexes serving foreign keys are kept.
        for kept in ["child_pkey", "parent_code_key", "child_other", "pair_ref_yx", "pair_partial_x", "big_pkey"] {
            assert!(listed(kept).is_empty(), "{kept}: {:?}", listed(kept));
        }
        let scans = report
            .items
            .iter()
            .find(|i| i.kind == AdviceKind::SequentialScans && i.table == "big")
            .expect("big is read by sequential scans");
        assert!(
            scans.detail.contains("sequential scans read")
                && scans.detail.ends_with("0 index scans; about 30000 rows."),
            "{}",
            scans.detail
        );
        let dead = report
            .items
            .iter()
            .find(|i| i.kind == AdviceKind::DeadRows && i.table == "churn")
            .expect("churn has dead rows");
        assert!(dead.detail.starts_with("15000 dead and 5000 live rows"), "{}", dead.detail);
        assert_eq!(dead.fix.as_deref(), Some("VACUUM (ANALYZE) \"public\".\"churn\""));
        assert!(report.items.windows(2).all(|w| w[0].kind <= w[1].kind));
        // The suggested SQL runs as written.
        for fix in [
            duplicate.fix.clone().unwrap(),
            fk.fix.clone().unwrap(),
            dead.fix.clone().unwrap(),
        ] {
            db.client.batch_execute(&fix).await?;
        }
        let after = db.index_report().await?;
        assert!(after.items.iter().all(|i| i.object != "child_a_copy"));
        assert!(!after
            .items
            .iter()
            .any(|i| i.kind == AdviceKind::ForeignKeyWithoutIndex && i.table == "child"));

        // pg_stat_statements is preloaded by the test cluster but not created in
        // a new database.
        let version: i32 = db
            .client
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .await?
            .get(0);
        let report = db.slow_queries(SlowQueryRequest::default()).await?;
        let StatementsStatus::Setup(setup) = &report.status else {
            panic!("{:?}", report.status);
        };
        assert!(setup.preloaded && !setup.installed, "{setup:?}");
        assert_eq!(setup.configure_blocker, None);
        db.enable_query_stats().await?;
        for n in [10, 20, 30] {
            db.client
                .query(
                    "SELECT count(*) FROM generate_series(1, $1::int) AS pgm_marker(n)",
                    &[&n],
                )
                .await?;
        }
        let report = db.slow_queries(SlowQueryRequest::default()).await?;
        assert_eq!(report.status, StatementsStatus::Ready);
        assert_eq!(report.generic_plans, version >= 160000);
        let marker = report
            .queries
            .iter()
            .find(|q| q.query.contains("pgm_marker"))
            .unwrap_or_else(|| panic!("marker query missing: {:#?}", report.queries));
        assert_eq!(marker.calls, 3);
        assert!(marker.query.contains("$1"), "{}", marker.query);
        assert!(marker.total_ms >= marker.mean_ms && marker.max_ms >= marker.mean_ms);
        assert!(report.queries.iter().all(|q| q.database == name));
        assert!(report.queries.iter().all(|q| !q.query.contains("pg_catalog.")));
        assert!(report.queries.windows(2).all(|w| w[0].total_ms >= w[1].total_ms));
        let share: f64 = report.queries.iter().map(|q| q.share).sum();
        assert!(share > 0.0 && share <= 100.001, "{share}");
        let by_calls = db
            .slow_queries(SlowQueryRequest {
                order: SlowQueryOrder::Calls,
                include_catalog: true,
                all_databases: true,
            })
            .await?;
        assert!(by_calls.queries.windows(2).all(|w| w[0].calls >= w[1].calls));
        assert!(by_calls.queries.iter().any(|q| q.query.contains("pg_catalog.")));
        if version >= 160000 {
            // Statements recorded with parameters can be explained as generic plans.
            let output = db
                .query("EXPLAIN (GENERIC_PLAN, FORMAT JSON) SELECT * FROM big WHERE id = $1")
                .await?;
            assert!(output.error.is_none(), "{:?}", output.error);
            let plan =
                postern::ui::explain::parse(output.results[0].rows[0][0].as_deref().unwrap())
                    .unwrap();
            assert!(plan.root.title.contains("big"), "{}", plan.root.title);
        }
        db.reset_query_stats().await?;
        let after = db.slow_queries(SlowQueryRequest::default()).await?;
        assert!(!after.queries.iter().any(|q| q.query.contains("pgm_marker")));
        assert!(after.stats_since.is_some());
        anyhow::Ok(())
    }
    .await;
    admin
        .client
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await?;
    result
}

#[test]
#[ignore = "restarts its server; scripts/test-postgres.sh runs it on a separate disposable cluster"]
fn query_statistics_setup_restarts_a_local_server() -> anyhow::Result<()> {
    use postern::services::{
        server::LocalServer,
        worker::{Command, Event, Worker},
    };
    fn wait(worker: &Worker) -> Event {
        let start = std::time::Instant::now();
        loop {
            if let Some(event) = worker.poll() {
                if event.finishes() {
                    return event;
                }
                continue;
            }
            assert!(start.elapsed().as_secs() < 120, "Worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let port = std::env::var("POSTERN_SETUP_PORT")
        .expect("Set POSTERN_SETUP_PORT to a disposable server that may be restarted");
    let target = ConnectionProfile {
        port: port.parse()?,
        ..profile()
    };
    let runtime = tokio::runtime::Runtime::new()?;
    // The script starts this server the way Postgres.app does, with
    // shared_preload_libraries on the command line.
    let db = runtime.block_on(Database::connect(target.clone(), ""))?;
    let facts = runtime.block_on(db.preload_facts())?;
    assert_eq!(
        facts.running.as_deref(),
        Some(&["auto_explain".to_owned()][..])
    );
    assert!(facts.superuser && facts.available && !facts.installed);
    let report = runtime.block_on(db.slow_queries(SlowQueryRequest::default()))?;
    let StatementsStatus::Setup(setup) = &report.status else {
        panic!("{:?}", report.status);
    };
    assert!(
        !setup.preloaded && !setup.installed && !setup.configured,
        "{setup:?}"
    );
    assert_eq!(setup.configure_blocker, None);
    let RestartSupport::Available {
        data_directory,
        log_file,
        ..
    } = &setup.restart
    else {
        panic!("{:?}", setup.restart);
    };
    assert!(log_file.ends_with("postgresql.log"));

    // A library the server cannot load stops it from starting; it is started
    // again with its previous command line.
    let local = runtime
        .block_on(LocalServer::detect(data_directory, facts.backend_pid))
        .map_err(anyhow::Error::msg)?;
    assert!(local.preload_on_command_line());
    drop(db);
    let failed = runtime
        .block_on(local.restart_with_library("pgm_missing_library", &["auto_explain".into()]));
    let message = format!("{:#}", failed.unwrap_err());
    assert!(
        message.contains("started again with its previous settings"),
        "{message}"
    );
    let db = runtime.block_on(Database::connect(target.clone(), ""))?;
    let facts = runtime.block_on(db.preload_facts())?;
    assert_eq!(facts.running.unwrap(), ["auto_explain"]);
    // Another application's session is counted before the restart.
    let report = runtime.block_on(db.slow_queries(SlowQueryRequest::default()))?;
    let StatementsStatus::Setup(StatementsSetup {
        restart: RestartSupport::Available { other_sessions, .. },
        ..
    }) = report.status
    else {
        panic!("{:?}", report.status);
    };
    assert_eq!(other_sessions, 0);
    drop(db);

    let worker = Worker::start(|| {})?;
    worker.send(Command::Connect(target.clone(), String::new()))?;
    assert!(matches!(wait(&worker), Event::Connected(..)));
    // An open transaction in a SQL document blocks the restart.
    let document = uuid::Uuid::new_v4();
    worker.send(Command::QueryIn(document, "BEGIN".into()))?;
    assert!(matches!(wait(&worker), Event::Query(_)));
    worker.send(Command::SetUpQueryStats(SlowQueryRequest::default()))?;
    match wait(&worker) {
        Event::Error(message) => assert!(
            message.contains("COMMIT or ROLLBACK before restarting the server"),
            "{message}"
        ),
        _ => panic!("an open transaction must block the restart"),
    }
    worker.send(Command::QueryIn(document, "ROLLBACK".into()))?;
    assert!(matches!(wait(&worker), Event::Query(_)));
    // Configure, restart, reconnect, and install in one command.
    worker.send(Command::SetUpQueryStats(SlowQueryRequest::default()))?;
    let report = match wait(&worker) {
        Event::SlowQueries(report) => report,
        Event::Error(message) => panic!("{message}"),
        _ => panic!("unexpected event"),
    };
    assert_eq!(report.status, StatementsStatus::Ready);
    // Document, shared, and data sessions work after the restart.
    worker.send(Command::QueryIn(
        document,
        "SHOW shared_preload_libraries".into(),
    ))?;
    let Event::Query(output) = wait(&worker) else {
        panic!("the document session must reconnect");
    };
    assert!(output.error.is_none(), "{:?}", output.error);
    assert_eq!(
        output.results[0].rows[0][0].as_deref(),
        Some("auto_explain,pg_stat_statements")
    );
    worker.send(Command::Query("SELECT 1".into()))?;
    assert!(matches!(wait(&worker), Event::Query(_)));
    worker.send(Command::SlowQueries(SlowQueryRequest::default()))?;
    let Event::SlowQueries(report) = wait(&worker) else {
        panic!("the data session must reconnect");
    };
    assert_eq!(report.status, StatementsStatus::Ready);
    // ALTER SYSTEM recorded the library, so the server's own launcher loads it
    // at its next start too.
    let db = runtime.block_on(Database::connect(target, ""))?;
    let facts = runtime.block_on(db.preload_facts())?;
    let configured = facts.configured.unwrap();
    assert_eq!(configured.libraries, ["pg_stat_statements"]);
    assert!(configured.in_auto_conf);
    Ok(())
}
