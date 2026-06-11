use std::collections::HashSet;
use std::path::PathBuf;

use turso::{Builder, Connection};

/// A history entry as stored in the database.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    /// Unix timestamp of the most recent visit (reserved for future display).
    #[allow(dead_code)]
    pub last_visited: i64,
}

/// Sort order for history queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistorySort {
    #[default]
    RecentFirst,
    MostVisited,
}

/// Path to the shared profile directory under the XDG data directory.
///
/// Creates the directory if it does not exist. Servo uses this path for
/// `cookie_jar.json`, `hsts_list.json`, and `auth_cache.json`.
pub fn profile_dir() -> PathBuf {
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cosmic-web");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Path to the browser database file under the XDG data directory.
pub fn db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cosmic-web")
        .join("browser.db")
}

/// Open (or create) the database at `path` and return a ready connection.
///
/// Creates the XDG data directory and schema if they do not exist yet.
pub async fn open(path: PathBuf) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path: {path:?}"))?;
    let db = Builder::new_local(path_str).build().await?;
    let conn = db.connect()?;
    init_schema(&conn).await?;
    Ok(conn)
}

async fn init_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS history (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            url         TEXT    NOT NULL UNIQUE,
            title       TEXT    NOT NULL DEFAULT '',
            visit_count INTEGER NOT NULL DEFAULT 1,
            last_visited INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS bookmarks (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            url    TEXT NOT NULL UNIQUE,
            title  TEXT NOT NULL DEFAULT '',
            folder TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS search_history (
            query         TEXT    NOT NULL PRIMARY KEY,
            last_searched INTEGER NOT NULL,
            search_count  INTEGER NOT NULL DEFAULT 1
        );",
    )
    .await?;

    // Migrate databases created before the `folder` column existed. SQLite has
    // no `ADD COLUMN IF NOT EXISTS`, so the statement errors with "duplicate
    // column" on already-migrated databases — that error is expected and ignored.
    let _ = conn
        .execute(
            "ALTER TABLE bookmarks ADD COLUMN folder TEXT NOT NULL DEFAULT ''",
            (),
        )
        .await;

    Ok(())
}

/// Record a page visit, upserting into history.
///
/// If the URL already exists the visit count is incremented and the title and
/// timestamp are updated. An empty `title` leaves the existing title unchanged.
pub async fn log_visit(conn: Connection, url: String, title: String) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO history (url, title, visit_count, last_visited)
         VALUES (?1, ?2, 1, unixepoch())
         ON CONFLICT(url) DO UPDATE SET
             title        = CASE WHEN excluded.title != '' THEN excluded.title
                                 ELSE history.title END,
             visit_count  = history.visit_count + 1,
             last_visited = excluded.last_visited",
        (url.as_str(), title.as_str()),
    )
    .await?;
    Ok(())
}

/// Record a search query, upserting into search_history.
///
/// Called whenever the user submits a search (not a URL navigation).
pub async fn log_search_query(conn: Connection, query: String) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO search_history (query, last_searched, search_count)
         VALUES (?1, unixepoch(), 1)
         ON CONFLICT(query) DO UPDATE SET
             last_searched = excluded.last_searched,
             search_count  = search_history.search_count + 1",
        [query.as_str()],
    )
    .await?;
    Ok(())
}

/// Query history entries, optionally filtered and sorted.
///
/// `filter` is matched as a substring against both URL and title (case-insensitive
/// via LIKE). Pass an empty string to return all entries up to `limit`.
pub async fn query_history(
    conn: Connection,
    filter: String,
    sort: HistorySort,
    limit: usize,
) -> anyhow::Result<Vec<HistoryEntry>> {
    let order = match sort {
        HistorySort::RecentFirst => "last_visited DESC",
        HistorySort::MostVisited => "visit_count DESC, last_visited DESC",
    };
    let sql = format!(
        "SELECT id, url, title, visit_count, last_visited
         FROM history
         WHERE url LIKE ?1 OR title LIKE ?1
         ORDER BY {order}
         LIMIT ?2"
    );
    let like = format!("%{filter}%");
    let mut rows = conn.query(&sql, (like.as_str(), limit as i64)).await?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await? {
        entries.push(HistoryEntry {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
            visit_count: row.get(3)?,
            last_visited: row.get(4)?,
        });
    }
    Ok(entries)
}

/// Delete a single history entry by its database id.
pub async fn delete_history_entry(conn: Connection, id: i64) -> anyhow::Result<()> {
    conn.execute("DELETE FROM history WHERE id = ?1", (id,))
        .await?;
    Ok(())
}

// ── Awesomebar suggestions ────────────────────────────────────────────────────

/// Source of an awesomebar suggestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Bookmark,
    History,
    /// Returned by the search engine's autocomplete API.
    Search,
    /// A previously typed search query retrieved from local search history.
    /// The `url` field on the parent `Suggestion` holds the raw query text, not a URL.
    SavedQuery,
}

/// A single awesomebar suggestion shown while the user types in the URL bar.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub url: String,
    pub title: String,
    pub kind: SuggestionKind,
}

/// Return up to `limit` suggestions matching `input`, bookmarks first.
///
/// Bookmarks are always ranked above history. History entries are ranked by a
/// recency × frequency score. Duplicates (bookmark + history) are deduplicated.
pub async fn query_suggestions(
    conn: Connection,
    input: String,
    limit: usize,
) -> anyhow::Result<Vec<Suggestion>> {
    let like = format!("%{input}%");

    // Bookmarks first.
    let mut rows = conn
        .query(
            "SELECT url, title FROM bookmarks
             WHERE url LIKE ?1 OR title LIKE ?1
             ORDER BY title
             LIMIT ?2",
            (like.as_str(), limit as i64),
        )
        .await?;

    let mut suggestions: Vec<Suggestion> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    while let Some(row) = rows.next().await? {
        let url: String = row.get(0)?;
        let title: String = row.get(1)?;
        seen.insert(url.clone());
        suggestions.push(Suggestion {
            url,
            title,
            kind: SuggestionKind::Bookmark,
        });
    }

    // Saved search queries ranked by recency × frequency.
    if suggestions.len() < limit {
        let mut rows = conn
            .query(
                "SELECT query FROM search_history
                 WHERE query LIKE ?1
                 ORDER BY CAST(search_count AS REAL)
                          / (1.0 + (unixepoch() - last_searched) / 86400.0) DESC
                 LIMIT ?2",
                (like.as_str(), limit as i64),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            if suggestions.len() >= limit {
                break;
            }
            let query: String = row.get(0)?;
            suggestions.push(Suggestion {
                url: query.clone(),
                title: query,
                kind: SuggestionKind::SavedQuery,
            });
        }
    }

    // History ranked by recency × frequency, deduped against bookmarks.
    if suggestions.len() < limit {
        let mut rows = conn
            .query(
                "SELECT url, title FROM history
                 WHERE url LIKE ?1 OR title LIKE ?1
                 ORDER BY CAST(visit_count AS REAL)
                          / (1.0 + (unixepoch() - last_visited) / 86400.0) DESC
                 LIMIT ?2",
                (like.as_str(), limit as i64),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            if suggestions.len() >= limit {
                break;
            }
            let url: String = row.get(0)?;
            if seen.contains(&url) {
                continue;
            }
            let title: String = row.get(1)?;
            suggestions.push(Suggestion {
                url,
                title,
                kind: SuggestionKind::History,
            });
        }
    }

    Ok(suggestions)
}

// ── Bookmarks ─────────────────────────────────────────────────────────────────

/// Reserved `folder` value for sites pinned to the New Tab page. A URL is unique
/// across the bookmarks table, so a site is either a normal bookmark (`folder =
/// ""`) or a pinned site (`folder = "pinned"`), never both at once.
pub const PINNED_FOLDER: &str = "pinned";

/// A bookmark entry as stored in the database.
#[derive(Clone, Debug)]
pub struct BookmarkEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
}

/// Add a bookmark, ignoring duplicates.
pub async fn add_bookmark(conn: Connection, url: String, title: String) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO bookmarks (url, title) VALUES (?1, ?2)",
        (url.as_str(), title.as_str()),
    )
    .await?;
    Ok(())
}

/// Pin a site to the New Tab page.
///
/// Stored as a bookmark in the reserved [`PINNED_FOLDER`]. If the URL already
/// exists as a normal bookmark it is moved into the pinned folder in place.
pub async fn add_pinned(conn: Connection, url: String, title: String) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO bookmarks (url, title, folder) VALUES (?1, ?2, ?3)
         ON CONFLICT(url) DO UPDATE SET
             folder = ?3,
             title  = CASE WHEN excluded.title != '' THEN excluded.title
                           ELSE bookmarks.title END",
        (url.as_str(), title.as_str(), PINNED_FOLDER),
    )
    .await?;
    Ok(())
}

/// Unpin a site from the New Tab page. Only removes rows in the pinned folder.
pub async fn remove_pinned(conn: Connection, url: String) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM bookmarks WHERE url = ?1 AND folder = ?2",
        (url.as_str(), PINNED_FOLDER),
    )
    .await?;
    Ok(())
}

/// Query the sites pinned to the New Tab page, ordered by title.
pub async fn query_pinned(conn: Connection) -> anyhow::Result<Vec<BookmarkEntry>> {
    let mut rows = conn
        .query(
            "SELECT id, url, title FROM bookmarks WHERE folder = ?1 ORDER BY title",
            (PINNED_FOLDER,),
        )
        .await?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await? {
        entries.push(BookmarkEntry {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
        });
    }
    Ok(entries)
}

/// Remove a bookmark by URL.
pub async fn remove_bookmark(conn: Connection, url: String) -> anyhow::Result<()> {
    conn.execute("DELETE FROM bookmarks WHERE url = ?1", (url.as_str(),))
        .await?;
    Ok(())
}

/// Delete a single bookmark by its database id.
pub async fn delete_bookmark_entry(conn: Connection, id: i64) -> anyhow::Result<()> {
    conn.execute("DELETE FROM bookmarks WHERE id = ?1", (id,))
        .await?;
    Ok(())
}

/// Query normal bookmarks filtered by URL or title substring, ordered by title.
///
/// Pinned sites (`folder = "pinned"`) are excluded so they do not clutter the
/// bookmarks drawer; they are shown only on the New Tab page via [`query_pinned`].
pub async fn query_bookmarks(
    conn: Connection,
    filter: String,
) -> anyhow::Result<Vec<BookmarkEntry>> {
    let like = format!("%{filter}%");
    let mut rows = conn
        .query(
            "SELECT id, url, title FROM bookmarks
             WHERE (url LIKE ?1 OR title LIKE ?1) AND folder = ''
             ORDER BY title",
            (like.as_str(),),
        )
        .await?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await? {
        entries.push(BookmarkEntry {
            id: row.get(0)?,
            url: row.get(1)?,
            title: row.get(2)?,
        });
    }
    Ok(entries)
}

/// Load the set of all bookmarked URLs (used for the star-icon check).
///
/// Includes pinned sites: they live in the bookmarks table, so the star
/// reflects presence there regardless of folder.
pub async fn load_bookmarked_urls(conn: Connection) -> anyhow::Result<HashSet<String>> {
    let mut rows = conn.query("SELECT url FROM bookmarks", ()).await?;
    let mut urls = HashSet::new();
    while let Some(row) = rows.next().await? {
        urls.insert(row.get::<String>(0)?);
    }
    Ok(urls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Open a fresh on-disk database in a unique temp path.
    ///
    /// turso's `Builder::new_local` expects a file path, so each test gets its
    /// own file that is removed first to guarantee a clean schema.
    async fn temp_db() -> anyhow::Result<Connection> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cosmic-web-test-{}-{n}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        open(path).await
    }

    #[test]
    fn pin_round_trip() -> anyhow::Result<()> {
        pollster::block_on(async {
            let conn = temp_db().await?;
            add_pinned(conn.clone(), "https://a.example/".into(), "A".into()).await?;

            let pinned = query_pinned(conn.clone()).await?;
            assert_eq!(pinned.len(), 1);
            assert_eq!(pinned[0].url, "https://a.example/");

            remove_pinned(conn.clone(), "https://a.example/".into()).await?;
            assert!(query_pinned(conn).await?.is_empty());
            Ok(())
        })
    }

    #[test]
    fn pinned_excluded_from_bookmarks_drawer() -> anyhow::Result<()> {
        pollster::block_on(async {
            let conn = temp_db().await?;
            add_bookmark(
                conn.clone(),
                "https://normal.example/".into(),
                "Normal".into(),
            )
            .await?;
            add_pinned(
                conn.clone(),
                "https://pinned.example/".into(),
                "Pinned".into(),
            )
            .await?;

            // The drawer lists only normal bookmarks…
            let drawer = query_bookmarks(conn.clone(), String::new()).await?;
            assert_eq!(drawer.len(), 1);
            assert_eq!(drawer[0].url, "https://normal.example/");

            // …while the New Tab page lists only pinned ones…
            let pinned = query_pinned(conn.clone()).await?;
            assert_eq!(pinned.len(), 1);
            assert_eq!(pinned[0].url, "https://pinned.example/");

            // …and the star check sees both (they share the bookmarks table).
            let urls = load_bookmarked_urls(conn).await?;
            assert!(urls.contains("https://normal.example/"));
            assert!(urls.contains("https://pinned.example/"));
            Ok(())
        })
    }

    #[test]
    fn pinning_existing_bookmark_moves_it() -> anyhow::Result<()> {
        pollster::block_on(async {
            let conn = temp_db().await?;
            add_bookmark(conn.clone(), "https://x.example/".into(), "X".into()).await?;
            // Pinning a URL that is already a normal bookmark converts it in place.
            add_pinned(conn.clone(), "https://x.example/".into(), "X".into()).await?;

            assert!(
                query_bookmarks(conn.clone(), String::new())
                    .await?
                    .is_empty()
            );
            assert_eq!(query_pinned(conn).await?.len(), 1);
            Ok(())
        })
    }

    #[test]
    fn schema_migration_is_idempotent() -> anyhow::Result<()> {
        pollster::block_on(async {
            // open() already ran init_schema once; running it again must not fail
            // even though the `folder` column already exists.
            let conn = temp_db().await?;
            init_schema(&conn).await?;
            init_schema(&conn).await?;
            Ok(())
        })
    }
}
