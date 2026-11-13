//! Scale check: the lists must stay windows over SQLite, not copies of it.
//!
//! 50 000 rows per table is the size the finalization plan calls out. The test
//! inserts them in one transaction, then reads windows from the top, the middle
//! and the bottom and checks that the page cache stays small.

use std::path::PathBuf;

use voice_button::config::AppDirs;
use voice_button::history;
use voice_button::paging::{Paged, PAGE};
use voice_button::site::db::{message_page, Db};
use voice_button::store::Store;

fn root() -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("vb-scale-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn fifty_thousand_rows_are_read_by_window() {
    let root = root();
    {
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Scale", "2026-11-12T10:00:00+05:00");
        db.save().unwrap();
        let store = Store::open(&root).unwrap();
        let mut conn = store.conn();
        let transaction = conn.transaction().unwrap();
        for index in 0..50_000u64 {
            transaction
                .execute(
                    voice_button::sql::query("store/test_scale_history_insert"),
                    rusqlite::params![
                        format!(
                            "2026-11-12 {:02}:{:02}:{:02}",
                            index / 3600,
                            (index / 60) % 60,
                            index % 60
                        ),
                        format!("row {index}")
                    ],
                )
                .unwrap();
            transaction
                .execute(
                    voice_button::sql::query("store/test_scale_chat_insert"),
                    rusqlite::params![format!("telegram row {index}")],
                )
                .unwrap();
            transaction
                .execute(
                    voice_button::sql::query("store/test_scale_message_insert"),
                    rusqlite::params![
                        index as i64 + 1,
                        monolog.id as i64,
                        format!(
                            "2026-11-12T{:02}:{:02}:{:02}+05:00",
                            index / 3600,
                            (index / 60) % 60,
                            index % 60
                        ),
                        format!("take {index}")
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }

    let history = history::History::load(&root);
    assert_eq!(history.len(), 50_000);
    let page = history.page(&[], "", voice_button::dates::DateFilter::everything(), 0, PAGE);
    assert_eq!(page.len(), PAGE);
    assert_eq!(page[0].preview(), "row 49999");
    let tail = history.page(&[], "", voice_button::dates::DateFilter::everything(), 49_800, PAGE);
    assert_eq!(tail[0].preview(), "row 199", "newest first: the tail is the oldest");
    assert_eq!(history.count(&[], "row 49999", voice_button::dates::DateFilter::everything()), 1);
    assert_eq!(history.count(&[], "", voice_button::dates::DateFilter::everything()), 50_000);

    let dirs = AppDirs {
        root: root.clone(),
        models: PathBuf::new(),
    };
    assert_eq!(voice_button::telegram::chat_count(&dirs, voice_button::dates::DateFilter::everything()), 50_000);
    let chat = voice_button::telegram::chat_page(&dirs, voice_button::dates::DateFilter::everything(), 200, 49_800);
    assert_eq!(chat.len(), 200);
    assert_eq!(chat[0].text, "telegram row 49800");

    let store = Store::open(&root).unwrap();
    let messages = message_page(&store, 1, 49_800, 200);
    assert_eq!(messages.len(), 200);
    assert_eq!(messages[0].text, "take 49800");

    // Scrolling to the end and back keeps three pages at most.
    let mut paged: Paged<usize> = Paged::new();
    paged.reset(50_000);
    paged.ensure(0..30, |offset, limit| (offset..offset + limit).collect());
    assert!(paged.cached_rows() <= 3 * PAGE);
    paged.ensure(49_970..49_990, |offset, limit| (offset..offset + limit).collect());
    assert_eq!(paged.get(49_980), Some(&49_980));
    assert!(
        paged.cached_rows() <= 3 * PAGE,
        "the cache grew to {} rows",
        paged.cached_rows()
    );
    assert_eq!(paged.get(0), None, "the first page was dropped");

    let _ = std::fs::remove_dir_all(root);
}
