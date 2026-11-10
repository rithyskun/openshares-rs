//! Repository tests — every test gets a fresh migrated SQLite file
//! (`Db::connect` + `Db::migrate` on a `tempfile`, covering the whole
//! `migrations/` chain end-to-end).

use tempfile::TempDir;

use crate::db::repos::{
    albums, friends, link_codes, media, rate_limits, roles, sessions, shares, telegram_users, users,
};
use crate::db::{Db, PUBLIC_LINK_TTL, now};

/// Temp-dir + migrated db handle. The `TempDir` keeps the file alive for
/// the test's duration.
async fn test_db() -> (Db, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}", dir.path().join("test.db").display());
    let db = Db::connect(&url).await.expect("connect");
    db.migrate().await.expect("migrate");
    (db, dir)
}

async fn make_user(db: &Db, name: &str) -> i64 {
    users::create(
        db,
        &users::NewUser {
            email: None,
            name,
            password_hash: None,
        },
    )
    .await
    .expect("create user")
}

async fn make_media(db: &Db, slug: &str, owner: i64) -> i64 {
    media::insert(
        db,
        &media::NewMedia {
            slug,
            owner_user_id: Some(owner),
            telegram_id: "42",
            kind: "photo",
            file_id: "file-1",
            availability: "available",
            ..Default::default()
        },
    )
    .await
    .expect("insert media")
}

/* ------------------------------- users -------------------------------- */

#[tokio::test]
async fn users_roundtrip_and_updates() {
    let (db, _t) = test_db().await;

    let id = users::create(
        &db,
        &users::NewUser {
            email: Some("Ana@Example.com"),
            name: "Ana",
            password_hash: Some("salt:hash"),
        },
    )
    .await
    .expect("create");

    // get_by_id + case-insensitive email lookup
    let u = users::get_by_id(&db, id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(u.name, "Ana");
    assert_eq!(u.email.as_deref(), Some("Ana@Example.com"));
    assert!(u.disabled_at.is_none());
    let by_email = users::get_by_email(&db, "ana@example.com")
        .await
        .expect("by email")
        .expect("exists");
    assert_eq!(by_email.id, id);

    // dup-check variants (create vs edit-excluding-self)
    assert!(
        users::email_taken(&db, "ANA@EXAMPLE.COM", None)
            .await
            .unwrap()
    );
    assert!(
        !users::email_taken(&db, "ana@example.com", Some(id))
            .await
            .unwrap()
    );
    assert!(!users::email_taken(&db, "other@x.io", None).await.unwrap());

    // profile updates + suspend/unsuspend
    users::update_name(&db, id, "Ana B").await.unwrap();
    users::update_email(&db, id, Some("b@x.io")).await.unwrap();
    users::update_password_hash(&db, id, "s2:h2").await.unwrap();
    users::disable(&db, id).await.unwrap();
    let u = users::get_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(u.name, "Ana B");
    assert_eq!(u.email.as_deref(), Some("b@x.io"));
    assert_eq!(u.password_hash.as_deref(), Some("s2:h2"));
    assert!(u.disabled_at.is_some());
    users::enable(&db, id).await.unwrap();
    assert!(
        users::get_by_id(&db, id)
            .await
            .unwrap()
            .unwrap()
            .disabled_at
            .is_none()
    );

    // list_all ordering by created_at
    let second = make_user(&db, "Bo").await;
    let all = users::list_all(&db).await.unwrap();
    assert!(all.len() >= 2);
    assert_eq!(all[0].id, id);
    assert_eq!(all[1].id, second);
}

#[tokio::test]
async fn user_roles_assign_list_remove() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    let editor = roles::get_by_name(&db, "editor").await.unwrap().unwrap();
    let viewer = roles::get_by_name(&db, "viewer").await.unwrap().unwrap();

    assert!(users::assign_role(&db, uid, editor.id).await.unwrap());
    // OR IGNORE — second assign reports false
    assert!(!users::assign_role(&db, uid, editor.id).await.unwrap());
    users::assign_role(&db, uid, viewer.id).await.unwrap();

    let names = users::role_names(&db, uid).await.unwrap();
    assert_eq!(names, vec!["editor".to_string(), "viewer".to_string()]);

    let all = users::all_user_role_names(&db).await.unwrap();
    assert!(all.iter().any(|r| r.user_id == uid && r.name == "editor"));

    users::remove_role(&db, uid, editor.id).await.unwrap();
    assert_eq!(users::role_names(&db, uid).await.unwrap(), vec!["viewer"]);
}

/* ------------------------------ sessions ------------------------------ */

#[tokio::test]
async fn sessions_lifecycle() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;

    sessions::create(&db, "sid-1", uid, sessions::SESSION_TTL)
        .await
        .unwrap();
    sessions::create(&db, "sid-2", uid, sessions::SESSION_TTL)
        .await
        .unwrap();

    // get_user join — valid sid+user resolves
    let row = sessions::get_user(&db, "sid-1", uid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, uid);
    assert_eq!(row.name, "Ana");
    assert!(row.session_created_at.is_some());
    // wrong user / unknown sid → None
    assert!(
        sessions::get_user(&db, "sid-1", uid + 9)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        sessions::get_user(&db, "nope", uid)
            .await
            .unwrap()
            .is_none()
    );

    // suspended user resolves to None (the disabled_at guard)
    users::disable(&db, uid).await.unwrap();
    assert!(
        sessions::get_user(&db, "sid-1", uid)
            .await
            .unwrap()
            .is_none()
    );
    users::enable(&db, uid).await.unwrap();

    // revoke-all-but keeps only the named session
    sessions::delete_all_except(&db, uid, "sid-2")
        .await
        .unwrap();
    assert!(
        sessions::get_user(&db, "sid-1", uid)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        sessions::get_user(&db, "sid-2", uid)
            .await
            .unwrap()
            .is_some()
    );

    sessions::delete(&db, "sid-2").await.unwrap();
    assert!(
        sessions::get_user(&db, "sid-2", uid)
            .await
            .unwrap()
            .is_none()
    );

    // expired sessions are swept + fail get_user
    db.execute(
        "INSERT INTO sessions (id, user_id, expires_at, created_at) VALUES ('old', ?, 1, 1)",
        libsql::params![uid],
    )
    .await
    .unwrap();
    assert!(sessions::get_user(&db, "old", uid).await.unwrap().is_none());
    sessions::cleanup_expired(&db).await.unwrap();
}

/* -------------------------------- media -------------------------------- */

#[tokio::test]
async fn media_insert_get_update_delete() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    let id = make_media(&db, "slug-a", uid).await;

    let m = media::get_by_slug(&db, "slug-a").await.unwrap().unwrap();
    assert_eq!(m.id, id);
    assert_eq!(m.kind, "photo");
    assert_eq!(m.owner_user_id, Some(uid));
    assert_eq!(m.missing, 0);

    media::update_caption(&db, id, Some("hello")).await.unwrap();
    media::mark_missing(&db, id).await.unwrap();
    media::increment_shares_count(&db, id).await.unwrap();
    let m = media::get_by_slug(&db, "slug-a").await.unwrap().unwrap();
    assert_eq!(m.caption.as_deref(), Some("hello"));
    assert_eq!(m.missing, 1);
    assert_eq!(m.shares_count, 1);

    // IN-list lookup + hard delete
    make_media(&db, "slug-b", uid).await;
    let rows = media::get_many_by_slugs(&db, &["slug-a".into(), "slug-b".into(), "nope".into()])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(media::delete_by_id(&db, id).await.unwrap());
    assert!(!media::delete_by_id(&db, id).await.unwrap());
    assert!(media::get_by_slug(&db, "slug-a").await.unwrap().is_none());
}

#[tokio::test]
async fn media_vault_pagination_and_filters() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;

    // 3 vault items — same created_at second, so id order settles the page.
    for slug in ["m1", "m2", "m3"] {
        make_media(&db, slug, uid).await;
    }
    // one video, one filed-away (album) item, one missing
    media::insert(
        &db,
        &media::NewMedia {
            slug: "v1",
            owner_user_id: Some(uid),
            telegram_id: "42",
            kind: "video",
            file_id: "f",
            availability: "available",
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let album = albums::create(
        &db,
        &albums::NewAlbum {
            slug: "al",
            owner_user_id: uid,
            title: "T",
            token: "tok",
        },
    )
    .await
    .unwrap();
    let filed = make_media(&db, "filed", uid).await;
    media::set_album_id(&db, filed, Some(album)).await.unwrap();
    let gone = make_media(&db, "gone", uid).await;
    media::mark_missing(&db, gone).await.unwrap();

    // page 1: vault scope is (created_at DESC, id DESC) — newest first,
    // album-filed items excluded. 5 vault items: m1,m2,m3,v1,gone.
    let page = media::list(
        &db,
        &media::MediaListQuery {
            limit: 3,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 3);
    // newest item first either way (gone was inserted last)
    assert_eq!(page.items[0].slug, "gone");
    assert_eq!(page.items[0].availability, "dead");
    // ordering is (created_at DESC, id DESC) — verify the invariant, not
    // a fixed sequence (items can straddle a second boundary).
    let mut keys: Vec<(i64, i64)> = page.items.iter().map(|m| (m.created_at, m.id)).collect();
    keys.sort_unstable_by(|a, b| b.cmp(a));
    assert!(page.items.iter().map(|m| (m.created_at, m.id)).eq(keys));
    assert!(page.next_cursor.is_some());

    // page 2 via the keyset cursor — the remaining 2 items, no overlap.
    let cursor = media::parse_vault_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
    let page2 = media::list(
        &db,
        &media::MediaListQuery {
            limit: 3,
            cursor: Some(cursor),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page2.items.len(), 2);
    assert!(page2.next_cursor.is_none());
    let mut slugs: Vec<&str> = page
        .items
        .iter()
        .chain(page2.items.iter())
        .map(|m| m.slug.as_str())
        .collect();
    slugs.sort_unstable();
    assert_eq!(slugs, ["gone", "m1", "m2", "m3", "v1"]);

    // filters: kind, search (LIKE-escaped), missing-only
    let videos = media::list(
        &db,
        &media::MediaListQuery {
            kind: Some("video"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(videos.items.len(), 1);
    assert_eq!(videos.items[0].slug, "v1");

    media::update_caption(&db, gone, Some("lost file"))
        .await
        .unwrap();
    let found = media::list(
        &db,
        &media::MediaListQuery {
            search: Some("lost"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found.items.len(), 1);
    // a literal % in the query must not act as a wildcard
    let escaped = media::list(
        &db,
        &media::MediaListQuery {
            search: Some("100%"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(escaped.items.is_empty());
    let missing = media::list(
        &db,
        &media::MediaListQuery {
            missing_only: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(missing.items.len(), 1);
    assert_eq!(missing.items[0].slug, "gone");
}

#[tokio::test]
async fn media_album_scope_pagination() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    let album = albums::create(
        &db,
        &albums::NewAlbum {
            slug: "al",
            owner_user_id: uid,
            title: "T",
            token: "tok",
        },
    )
    .await
    .unwrap();
    for slug in ["a1", "a2", "a3", "a4"] {
        let id = make_media(&db, slug, uid).await;
        media::set_album_id(&db, id, Some(album)).await.unwrap();
    }
    make_media(&db, "vault", uid).await; // excluded

    // album scope is id ASC
    let page = media::list(
        &db,
        &media::MediaListQuery {
            scope: media::MediaScope::Album(album),
            limit: 2,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].slug, "a1");
    assert_eq!(page.items[1].slug, "a2");

    let cursor = media::parse_album_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
    let page2 = media::list(
        &db,
        &media::MediaListQuery {
            scope: media::MediaScope::Album(album),
            limit: 2,
            cursor: Some(cursor),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items[0].slug, "a3");

    assert_eq!(media::count_in_album(&db, album).await.unwrap(), 4);
    assert_eq!(
        media::first_live_slug_in_album(&db, album)
            .await
            .unwrap()
            .as_deref(),
        Some("a1")
    );
}

/* -------------------------------- albums ------------------------------- */

#[tokio::test]
async fn albums_crud_public_gate_and_counters() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    let id = albums::create(
        &db,
        &albums::NewAlbum {
            slug: "sum",
            owner_user_id: uid,
            title: "Summer",
            token: "tok-1",
        },
    )
    .await
    .unwrap();

    assert_eq!(
        albums::get_by_slug(&db, "sum").await.unwrap().unwrap().id,
        id
    );
    assert_eq!(
        albums::get_by_id(&db, id).await.unwrap().unwrap().title,
        "Summer"
    );
    assert_eq!(
        albums::get_by_token(&db, "tok-1")
            .await
            .unwrap()
            .unwrap()
            .id,
        id
    );

    // not public → gate closed
    let a = albums::get_by_id(&db, id).await.unwrap().unwrap();
    assert!(!albums::public_link_live(
        a.is_public,
        a.public_expires_at,
        now()
    ));

    // enable for the TTL window → live; past expiry → dead; revoke → dead
    albums::set_public(&db, id, true, Some(now() + PUBLIC_LINK_TTL))
        .await
        .unwrap();
    let a = albums::get_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(a.is_public, 1);
    assert!(albums::public_link_live(
        a.is_public,
        a.public_expires_at,
        now()
    ));
    assert!(!albums::public_link_live(1, Some(now() - 1), now()));
    albums::revoke_public(&db, id).await.unwrap();
    let a = albums::get_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(a.is_public, 0);

    // rename, counters (RETURNING likes), public head row
    albums::rename(&db, id, "Winter").await.unwrap();
    albums::increment_views(&db, id).await.unwrap();
    albums::increment_views(&db, id).await.unwrap();
    assert_eq!(albums::increment_likes(&db, id).await.unwrap(), Some(1));
    let head = albums::get_public_by_token(&db, "tok-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.title, "Winter");
    assert_eq!(head.likes, 1);
    assert_eq!(head.owner_name.as_deref(), Some("Ana"));

    // list stats: item_count + cover_slug from the media side
    let m1 = make_media(&db, "c1", uid).await;
    media::set_album_id(&db, m1, Some(id)).await.unwrap();
    let m2 = make_media(&db, "c2", uid).await;
    media::set_album_id(&db, m2, Some(id)).await.unwrap();
    let stats = albums::list_all_with_stats(&db).await.unwrap();
    let row = stats.iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.item_count, 2);
    assert_eq!(row.cover_slug.as_deref(), Some("c1"));
    assert_eq!(row.views, 2);

    // delete → media.album_id SET NULL back to the vault
    albums::delete_by_id(&db, id).await.unwrap();
    let m = media::get_by_slug(&db, "c1").await.unwrap().unwrap();
    assert!(m.album_id.is_none());
}

#[tokio::test]
async fn album_token_media_gate() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    let album = albums::create(
        &db,
        &albums::NewAlbum {
            slug: "al",
            owner_user_id: uid,
            title: "T",
            token: "tok",
        },
    )
    .await
    .unwrap();
    let mid = make_media(&db, "x", uid).await;
    media::set_album_id(&db, mid, Some(album)).await.unwrap();

    // private → no access
    assert!(!albums::token_allows_media(&db, mid, "tok").await.unwrap());
    // live public → access
    albums::set_public(&db, album, true, Some(now() + PUBLIC_LINK_TTL))
        .await
        .unwrap();
    assert!(albums::token_allows_media(&db, mid, "tok").await.unwrap());
    // wrong media / wrong token → no
    assert!(
        !albums::token_allows_media(&db, mid + 1, "tok")
            .await
            .unwrap()
    );
    assert!(!albums::token_allows_media(&db, mid, "other").await.unwrap());
}

/* -------------------------------- shares ------------------------------- */

#[tokio::test]
async fn shares_lifecycle_and_token_lookups() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    let other = make_user(&db, "Bo").await;
    let mid = make_media(&db, "m", uid).await;

    shares::create_link(&db, mid, "tok-a", uid, now() + PUBLIC_LINK_TTL)
        .await
        .unwrap();
    shares::create_link(&db, mid, "tok-b", other, now() + PUBLIC_LINK_TTL)
        .await
        .unwrap();
    shares::create_telegram(&db, mid, "777", uid).await.unwrap();

    // list: own-only vs see-all, expiry filtered
    let mine = shares::list_links(&db, mid, Some(uid)).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].token.as_deref(), Some("tok-a"));
    let all = shares::list_links(&db, mid, None).await.unwrap();
    assert_eq!(all.len(), 2);

    // authz checks + slug resolution
    assert!(shares::live_link_exists(&db, mid, "tok-a").await.unwrap());
    assert!(!shares::live_link_exists(&db, mid, "nope").await.unwrap());
    assert_eq!(
        shares::media_slug_by_link_token(&db, "tok-b")
            .await
            .unwrap()
            .as_deref(),
        Some("m")
    );

    // counters: /s page view + like RETURNING
    shares::increment_views(&db, "tok-a").await.unwrap();
    assert_eq!(
        shares::increment_likes(&db, "tok-a").await.unwrap(),
        Some(1)
    );

    // /s page row — media fields + owner name + share counters
    let row = shares::get_shared_media_by_token(&db, "tok-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.slug, "m");
    assert_eq!(row.owner_name.as_deref(), Some("Ana"));
    assert_eq!(row.likes, 1);
    let (likes, expires) = shares::get_link_like_state(&db, "tok-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(likes, 1);
    assert!(expires.is_some());

    // creator lookup (own-vs-admin revoke check) + delete
    assert_eq!(
        shares::get_link_creator(&db, mid, "tok-b")
            .await
            .unwrap()
            .flatten(),
        Some(other)
    );
    assert!(shares::delete_link(&db, mid, "tok-b").await.unwrap());
    assert!(!shares::delete_link(&db, mid, "tok-b").await.unwrap());
    assert!(!shares::live_link_exists(&db, mid, "tok-b").await.unwrap());

    // expired link is invisible to live lookups
    db.execute(
        "INSERT INTO shares (media_id, kind, token, created_by, expires_at, created_at)
         VALUES (?, 'link', 'dead-tok', ?, 1, 1)",
        libsql::params![mid, uid],
    )
    .await
    .unwrap();
    assert!(
        !shares::live_link_exists(&db, mid, "dead-tok")
            .await
            .unwrap()
    );
    assert!(
        shares::media_slug_by_link_token(&db, "dead-tok")
            .await
            .unwrap()
            .is_none()
    );
    // …but the /s page still resolves it (explicit "expired" state)
    assert!(
        shares::get_shared_media_by_token(&db, "dead-tok")
            .await
            .unwrap()
            .is_some()
    );
}

/* ------------------------------- friends ------------------------------- */

async fn make_telegram_user(db: &Db, tg_id: &str, username: Option<&str>) {
    db.execute(
        "INSERT INTO telegram_users (telegram_id, username, first_name, last_name, updated_at)
         VALUES (?, ?, 'F', 'L', 1)",
        libsql::params![tg_id, username],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn friends_add_list_remove() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;
    make_telegram_user(&db, "555", Some("pal")).await;

    assert!(friends::add(&db, uid, "555").await.unwrap());
    assert!(!friends::add(&db, uid, "555").await.unwrap()); // UNIQUE dedupe

    // telegram share bumps the `shared` subquery count
    let mid = make_media(&db, "m", uid).await;
    shares::create_telegram(&db, mid, "555", uid).await.unwrap();

    let rows = friends::list_for_owner(&db, uid).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].telegram_id, "555");
    assert_eq!(rows[0].username.as_deref(), Some("pal"));
    assert_eq!(rows[0].shared, 1);

    // share-target lookup: owner-gated
    let target = friends::get_link_target(&db, rows[0].id, uid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.telegram_id, "555");
    assert!(
        friends::get_link_target(&db, rows[0].id, uid + 9)
            .await
            .unwrap()
            .is_none()
    );

    // remove is owner-scoped too
    assert!(!friends::remove(&db, rows[0].id, uid + 9).await.unwrap());
    assert!(friends::remove(&db, rows[0].id, uid).await.unwrap());
    assert!(friends::list_for_owner(&db, uid).await.unwrap().is_empty());
}

/* ------------------------- link_codes / tg users ------------------------ */

#[tokio::test]
async fn link_codes_create_and_expiry() {
    let (db, _t) = test_db().await;
    let uid = make_user(&db, "Ana").await;

    link_codes::create(&db, "code-1", "friend", uid, Some(now() + 7 * 86400))
        .await
        .unwrap();
    let rows = db
        .query_rows(
            "SELECT purpose, expiry_at, used FROM link_codes WHERE code = 'code-1'",
            (),
        )
        .await
        .unwrap();
    let row = rows.first().expect("row");
    let (purpose, expiry, used) = (
        row.get::<String>(0).unwrap(),
        row.get::<Option<i64>>(1).unwrap(),
        row.get::<i64>(2).unwrap(),
    );
    assert_eq!(purpose, "friend");
    assert!(expiry.is_some());
    assert_eq!(used, 0);

    link_codes::set_expiry(&db, "code-1", None).await.unwrap();
    let expiry: Option<i64> = db
        .scalar::<Option<i64>>("SELECT expiry_at FROM link_codes WHERE code = 'code-1'", ())
        .await
        .unwrap();
    assert!(expiry.is_none());

    // telegram_users read path (written by the bot in production)
    make_telegram_user(&db, "555", Some("pal")).await;
    let tg = telegram_users::get_by_id(&db, "555")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tg.username.as_deref(), Some("pal"));
    assert!(
        telegram_users::get_by_id(&db, "999")
            .await
            .unwrap()
            .is_none()
    );
}

/* ----------------------------- rate_limits ----------------------------- */

#[tokio::test]
async fn rate_limits_fixed_window() {
    let (db, _t) = test_db().await;
    let t = 1_000_000i64;

    // hit() returns the post-increment count within one window
    assert_eq!(
        rate_limits::hit(&db, "AUTH_LIMITER:ip1", t, 60)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        rate_limits::hit(&db, "AUTH_LIMITER:ip1", t + 30, 60)
            .await
            .unwrap(),
        2
    );
    // different bucket → its own counter
    assert_eq!(
        rate_limits::hit(&db, "AUTH_LIMITER:ip2", t, 60)
            .await
            .unwrap(),
        1
    );

    let row = rate_limits::get(&db, "AUTH_LIMITER:ip1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.count, 2);
    assert_eq!(row.window_start, t);

    // past the window the counter resets to 1 and re-anchors
    assert_eq!(
        rate_limits::hit(&db, "AUTH_LIMITER:ip1", t + 61, 60)
            .await
            .unwrap(),
        1
    );
    let row = rate_limits::get(&db, "AUTH_LIMITER:ip1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.window_start, t + 61);

    // sweep clears stale rows only: ip2 (window_start = t) goes,
    // ip1 (re-anchored at t+61) survives.
    let swept = rate_limits::sweep(&db, t + 30).await.unwrap();
    assert_eq!(swept, 1);
    assert!(
        rate_limits::get(&db, "AUTH_LIMITER:ip1")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        rate_limits::get(&db, "AUTH_LIMITER:ip2")
            .await
            .unwrap()
            .is_none()
    );
}

/* -------------------------------- roles -------------------------------- */

#[tokio::test]
async fn roles_admin_surface() {
    let (db, _t) = test_db().await;

    // builtin roles seeded by migration 0003
    let all = roles::list(&db).await.unwrap();
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|r| r.builtin == 1));
    let admin = roles::get_by_name(&db, "admin").await.unwrap().unwrap();
    assert!(roles::has_wildcard(&db, admin.id).await.unwrap());

    // create → rename → clash check
    let rid = roles::create(&db, "auditor").await.unwrap();
    assert_eq!(
        roles::get_by_id(&db, rid).await.unwrap().unwrap().name,
        "auditor"
    );
    assert!(roles::name_taken(&db, "viewer", None).await.unwrap());
    assert!(
        !roles::name_taken(
            &db,
            "viewer",
            Some(roles::get_by_name(&db, "viewer").await.unwrap().unwrap().id,)
        )
        .await
        .unwrap()
    );
    roles::rename(&db, rid, "audit").await.unwrap();
    assert!(roles::get_by_name(&db, "auditor").await.unwrap().is_none());

    // permission replace is transactional: delete + insert per key
    roles::replace_permissions(&db, rid, &["media:view".into(), "album:create".into()])
        .await
        .unwrap();
    let perms = roles::all_permissions(&db).await.unwrap();
    let mine: Vec<&str> = perms
        .iter()
        .filter(|p| p.role_id == rid)
        .map(|p| p.permission_key.as_str())
        .collect();
    assert_eq!(mine, vec!["album:create", "media:view"]);
    roles::replace_permissions(&db, rid, &["media:view".into()])
        .await
        .unwrap();
    let perms = roles::all_permissions(&db).await.unwrap();
    assert_eq!(perms.iter().filter(|p| p.role_id == rid).count(), 1);

    // member counts + admin ids (with/without role exclusion)
    let uid = make_user(&db, "Ana").await;
    users::assign_role(&db, uid, admin.id).await.unwrap();
    users::assign_role(&db, uid, rid).await.unwrap();
    let counts = roles::member_counts(&db).await.unwrap();
    assert!(counts.iter().any(|c| c.role_id == admin.id && c.n == 1));
    assert!(
        roles::admin_user_ids(&db, None)
            .await
            .unwrap()
            .contains(&uid)
    );
    assert!(
        roles::admin_user_ids(&db, Some(admin.id))
            .await
            .unwrap()
            .is_empty()
    );

    roles::delete_by_id(&db, rid).await.unwrap();
    assert!(roles::get_by_id(&db, rid).await.unwrap().is_none());
}
