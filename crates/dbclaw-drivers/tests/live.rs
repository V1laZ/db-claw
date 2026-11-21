//! Integration tests against the docker-compose databases.
//! Run with: `docker compose up -d && DBCLAW_LIVE=1 cargo test -p dbclaw-drivers --test live`

use dbclaw_core::filter;
use dbclaw_core::model::*;
use dbclaw_drivers::{Connection, PgExecError};

fn live() -> bool {
    std::env::var("DBCLAW_LIVE").is_ok()
}

fn pg_config() -> ConnectionConfig {
    ConnectionConfig {
        id: "pg".into(),
        name: "pg".into(),
        kind: DbKind::Postgres,
        url: "postgres://dbclaw@localhost:54329/shop".into(),
        password: Some("dbclaw".into()),
        read_only: false,
    }
}

fn mongo_config() -> ConnectionConfig {
    ConnectionConfig {
        id: "mongo".into(),
        name: "mongo".into(),
        kind: DbKind::Mongo,
        url: "mongodb://dbclaw@localhost:27019/shop?authSource=admin".into(),
        password: Some("dbclaw".into()),
        read_only: false,
    }
}

fn find<'a>(objs: &'a [ObjectInfo], schema: &str, name: &str) -> &'a ObjectRef {
    &objs.iter().find(|o| o.object.schema == schema && o.object.name == name).unwrap().object
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_browse_filter_edit() {
    if !live() {
        return;
    }
    let conn = Connection::open(pg_config()).await.unwrap();
    assert!(conn.server_version().starts_with("PostgreSQL 17"));
    let dbs = conn.list_databases().await.unwrap();
    assert!(dbs.contains(&"shop".to_string()));
    let objs = conn.list_objects("shop").await.unwrap();
    let customers = find(&objs, "public", "customers").clone();
    assert_eq!(find(&objs, "analytics", "revenue_by_country").kind, ObjectKind::View);

    let cols = conn.columns(&customers).await.unwrap();
    let id = cols.iter().find(|c| c.name == "id").unwrap();
    assert!(id.is_pk && id.kind == ValueKind::Number);
    assert_eq!(cols.iter().find(|c| c.name == "vip").unwrap().kind, ValueKind::Bool);

    let req = PageRequest {
        object: customers.clone(),
        filter: filter::parse("country = \"CZ\" and profile.city = \"Brno\" and vip = false").unwrap(),
        sort: vec![SortSpec { column: "id".into(), dir: SortDir::Desc }],
        offset: 0,
        limit: 50,
    };
    let page = conn.fetch_page(&req, &cols).await.unwrap();
    assert!(!page.rows.is_empty());
    let n = conn.count(&req, &cols).await.unwrap();
    assert!(n >= page.rows.len() as u64);

    // Insert, update, delete in one transaction.
    let email = format!("live-test-{}@example.com", std::process::id());
    let summary = conn
        .apply(&customers, vec![RowChange::Insert {
            values: vec![
                ("id".into(), NewValue::Default),
                ("email".into(), NewValue::Text(Some(email.clone()))),
                ("name".into(), NewValue::Text(Some("O'Test".into()))),
                ("tags".into(), NewValue::Text(Some("{a,b}".into()))),
            ],
        }])
        .await
        .unwrap();
    assert_eq!(summary.inserted, 1);
    let req2 = PageRequest {
        filter: filter::parse(&format!("email = \"{email}\"")).unwrap(),
        ..req.clone()
    };
    let page = conn.fetch_page(&req2, &cols).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    let id_ix = cols.iter().position(|c| c.name == "id").unwrap();
    let new_id = page.rows[0][id_ix].as_str().unwrap().to_string();
    let key = RowKey::Pk(vec![("id".into(), Some(new_id))]);
    let s = conn
        .apply(&customers, vec![
            RowChange::Update {
                key: key.clone(),
                values: vec![("balance".into(), NewValue::Text(Some("12.5".into())))],
            },
            RowChange::Delete { key: key.clone() },
        ])
        .await
        .unwrap();
    assert_eq!((s.updated, s.deleted), (1, 1));

    // A failing change rolls the whole batch back.
    let err = conn
        .apply(&customers, vec![RowChange::Delete { key }])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exactly 1 row"), "{err}");

    // Table without primary key pages with ctid.
    let audit = find(&objs, "public", "audit_log").clone();
    let acols = conn.columns(&audit).await.unwrap();
    let page = conn
        .fetch_page(&PageRequest { object: audit, filter: None, sort: vec![], offset: 0, limit: 5 }, &acols)
        .await
        .unwrap();
    assert_eq!(page.ctids.as_ref().unwrap().len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_console() {
    if !live() {
        return;
    }
    let conn = Connection::open(pg_config()).await.unwrap();
    let session = conn.open_session("shop").await.unwrap();
    let out = session
        .execute("select id, email, vip, balance from customers order by id limit 3", 1000, None)
        .await
        .unwrap();
    assert_eq!(out.sets.len(), 1);
    let set = &out.sets[0];
    assert_eq!(set.rows.len(), 3);
    assert_eq!(set.columns[0].type_name, "int4");
    assert_eq!(set.columns[2].kind, ValueKind::Bool);

    let out = session
        .execute("begin; create temp table t(x int); insert into t values (1),(2); select * from t; rollback", 1000, None)
        .await
        .unwrap();
    assert_eq!(out.sets.len(), 5);
    assert_eq!(out.sets[2].affected, Some(2));
    assert_eq!(out.sets[3].rows.len(), 2);

    // Row limit.
    let out = session.execute("select * from analytics.events", 100, None).await.unwrap();
    assert!(out.sets[0].truncated);
    assert_eq!(out.sets[0].rows.len(), 100);

    // Error keeps earlier results.
    let err = session.execute("select 1; select * from nope", 100, None).await.unwrap_err();
    let pe = err.downcast_ref::<PgExecError>().unwrap();
    assert!(pe.message.contains("nope"), "{}", pe.message);
    assert_eq!(pe.partial.sets.len(), 1);

    // Notices.
    let out = session.execute("do $$ begin raise notice 'hello'; end $$", 100, None).await.unwrap();
    assert!(out.notices.iter().any(|n| n.contains("hello")), "{:?}", out.notices);
}

#[tokio::test(flavor = "multi_thread")]
async fn mongo_browse_filter_edit_console() {
    if !live() {
        return;
    }
    let conn = Connection::open(mongo_config()).await.unwrap();
    assert!(conn.server_version().starts_with("MongoDB"));
    assert_eq!(conn.default_database().as_deref(), Some("shop"));
    let dbs = conn.list_databases().await.unwrap();
    assert!(dbs.contains(&"shop".to_string()));
    let objs = conn.list_objects("shop").await.unwrap();
    let users = find(&objs, "", "users").clone();

    let req = PageRequest {
        object: users.clone(),
        filter: filter::parse("age >= 30 and address.city = \"Prague\" and tags in [\"beta\"]").unwrap(),
        sort: vec![SortSpec { column: "age".into(), dir: SortDir::Asc }],
        offset: 0,
        limit: 20,
    };
    let page = conn.fetch_page(&req, &[]).await.unwrap();
    assert_eq!(page.rows.len(), 20);
    assert_eq!(page.columns[0].name, "_id");
    assert!(conn.count(&req, &[]).await.unwrap() > 20);

    let session = conn.open_session("shop").await.unwrap();
    let out = session
        .execute("db.users.insertOne({email: 'live@x.cz', age: NumberInt(5), tags: []})", 10, None)
        .await
        .unwrap();
    let id = out.sets[0].documents.as_ref().unwrap()[0].get("insertedId").unwrap().clone();
    let key = RowKey::Id(id.clone());
    let s = conn
        .apply(&users, vec![
            RowChange::Update {
                key: key.clone(),
                values: vec![("age".into(), NewValue::Bson(bson::Bson::Int32(6))), ("tags".into(), NewValue::Unset)],
            },
        ])
        .await
        .unwrap();
    assert_eq!(s.updated, 1);
    let out = session.execute("{email: 'live@x.cz'}", 10, Some("users")).await.unwrap();
    let doc = &out.sets[0].documents.as_ref().unwrap()[0];
    assert_eq!(doc.get_i32("age").unwrap(), 6);
    assert!(!doc.contains_key("tags"));
    conn.apply(&users, vec![RowChange::Delete { key }]).await.unwrap();

    let out = session
        .execute("db.orders.aggregate([{$group: {_id: '$status', n: {$sum: 1}}}, {$sort: {_id: 1}}])", 100, None)
        .await
        .unwrap();
    assert_eq!(out.sets[0].rows.len(), 4);
    let out = session.execute("db.orders.countDocuments({status: 'paid'})", 100, None).await.unwrap();
    assert_eq!(out.sets[0].status.as_deref(), Some("count: 5000"));
}
