//! SQLite storage for clipboard history and notes.
//!
//! The daemon writes; the popup reads and deletes. WAL mode makes the two
//! processes play nicely with each other.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Clip,
    Note,
}

impl Kind {
    fn code(self) -> i64 {
        match self {
            Kind::Clip => 0,
            Kind::Note => 1,
        }
    }

    fn from_code(code: i64) -> Self {
        if code == 1 { Kind::Note } else { Kind::Clip }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub kind: Kind,
    pub text: String,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

pub struct Store {
    conn: Connection,
}

pub fn db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("collage")
        .join("collage.db")
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Store {
    pub fn open() -> Result<Self> {
        Self::open_at(db_path())
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                 id         INTEGER PRIMARY KEY,
                 kind       INTEGER NOT NULL,
                 text       TEXT    NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS entries_kind_time
                 ON entries (kind, created_at DESC, id DESC);",
        )?;
        Ok(Self { conn })
    }

    /// Add a clipboard entry. Identical older clips are removed so the item
    /// just moves to the top, and history is trimmed to `limit` entries.
    pub fn add_clip(&self, text: &str, limit: usize) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM entries WHERE kind = ?1 AND text = ?2",
            params![Kind::Clip.code(), text],
        )?;
        tx.execute(
            "INSERT INTO entries (kind, text, created_at) VALUES (?1, ?2, ?3)",
            params![Kind::Clip.code(), text, now()],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "DELETE FROM entries WHERE kind = ?1 AND id NOT IN (
                 SELECT id FROM entries WHERE kind = ?1
                 ORDER BY created_at DESC, id DESC LIMIT ?2)",
            params![Kind::Clip.code(), limit as i64],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn add_note(&self, text: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO entries (kind, text, created_at) VALUES (?1, ?2, ?3)",
            params![Kind::Note.code(), text, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, kind, text, created_at FROM entries WHERE id = ?1",
                params![id],
                row_to_entry,
            )
            .optional()?)
    }

    /// Newest first.
    pub fn list(&self, kind: Kind, limit: usize) -> Result<Vec<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, text, created_at FROM entries WHERE kind = ?1
             ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![kind.code(), limit as i64], row_to_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Bump a clip to the top of history (used when re-copying it).
    pub fn touch(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET created_at = ?2 WHERE id = ?1 AND kind = ?3",
            params![id, now(), Kind::Clip.code()],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear(&self, kind: Kind) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM entries WHERE kind = ?1", params![kind.code()])?)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: row.get(0)?,
        kind: Kind::from_code(row.get(1)?),
        text: row.get(2)?,
        created_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let path = std::env::temp_dir().join(format!(
            "collage-test-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        Store::open_at(path).unwrap()
    }

    #[test]
    fn clips_dedupe_and_trim() {
        let store = temp_store();
        store.add_clip("a", 3).unwrap();
        store.add_clip("b", 3).unwrap();
        store.add_clip("a", 3).unwrap(); // moves "a" to the top, no duplicate
        let texts = |s: &Store| s.list(Kind::Clip, 10).unwrap().into_iter().map(|e| e.text).collect::<Vec<_>>();
        assert_eq!(texts(&store), ["a", "b"]);
        store.add_clip("c", 3).unwrap();
        store.add_clip("d", 3).unwrap();
        assert_eq!(texts(&store), ["d", "c", "a"]);
    }

    #[test]
    fn notes_are_separate_and_kept() {
        let store = temp_store();
        let id = store.add_note("remember this").unwrap();
        store.add_note("remember this").unwrap(); // notes never dedupe
        store.add_clip("x", 1).unwrap();
        assert_eq!(store.list(Kind::Note, 10).unwrap().len(), 2);
        assert_eq!(store.clear(Kind::Clip).unwrap(), 1);
        assert_eq!(store.get(id).unwrap().unwrap().kind, Kind::Note);
        store.delete(id).unwrap();
        assert!(store.get(id).unwrap().is_none());
    }
}
