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
            id    INTEGER PRIMARY KEY AUTOINCREMENT,
            url   TEXT NOT NULL UNIQUE,
            title TEXT NOT NULL DEFAULT ''
        );",
    )
    .await?;
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
