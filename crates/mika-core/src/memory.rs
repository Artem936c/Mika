//! Long-term memory: facts about the user and search over past conversations.
//!
//! With an embedding model the search goes by meaning; without one, by words.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::params;
use serde::Serialize;

use crate::llm::Role;
use crate::storage::{now, Db, DbError};

/// Facts sent with each request, characters.
pub const FACTS_BUDGET: usize = 4000;
/// Longest single fact, characters.
pub const MAX_FACT: usize = 500;
/// Past messages looked at by one history search.
const HISTORY_WINDOW: usize = 2000;
/// New message vectors computed by one history search; the rest wait for the next one.
const EMBED_PER_SEARCH: usize = 256;
const EMBED_BATCH: usize = 64;
/// Longest text sent to the embedding model, characters.
const EMBED_CHARS: usize = 2000;
/// The reply does not wait for memory longer than this.
const RECALL_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub id: i64,
    pub created_at: String,
    pub text: String,
}

/// Turns texts into vectors. Lives in `mika-providers`; the core only uses it.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Model name, or nothing when embeddings are off. Vectors of different models never mix.
    fn model(&self) -> Option<String>;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// Adds remembered context to one reply. The agent calls it once per user message.
#[async_trait]
pub trait Recall: Send + Sync {
    async fn recall(&self, user_text: &str) -> Option<String>;
}

pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode_vector(bytes: &[u8]) -> Vec<f32> {
    // A trailing piece shorter than four bytes is ignored.
    let (whole, _) = bytes.as_chunks::<4>();
    whole.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// Cosine similarity; 0 for vectors of different length or zero length.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn clip(text: &str) -> String {
    text.chars().take(EMBED_CHARS).collect()
}

fn has_words(text: &str, words: &[String]) -> bool {
    let hay = text.to_lowercase();
    words.iter().all(|w| hay.contains(w))
}

fn words_of(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[derive(Clone)]
pub struct FactStore {
    db: Arc<Db>,
}

impl FactStore {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// Saves a fact. The same text twice gives the old id.
    pub fn add(&self, text: &str) -> Result<i64, DbError> {
        let text: String = text.trim().chars().take(MAX_FACT).collect();
        if let Some(f) = self
            .list()?
            .into_iter()
            .find(|f| f.text.to_lowercase() == text.to_lowercase())
        {
            return Ok(f.id);
        }
        self.db.with(|c| {
            c.execute(
                "INSERT INTO facts (created_at, text) VALUES (?1, ?2)",
                params![now(), text],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Oldest first.
    pub fn list(&self) -> Result<Vec<Fact>, DbError> {
        self.db.with(|c| {
            let mut st = c.prepare("SELECT id, created_at, text FROM facts ORDER BY id")?;
            let rows = st.query_map([], |r| {
                Ok(Fact {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    text: r.get(2)?,
                })
            })?;
            rows.collect()
        })
    }

    /// Facts that contain every word of the query, case ignored.
    pub fn search(&self, query: &str) -> Result<Vec<Fact>, DbError> {
        let words = words_of(query);
        Ok(self
            .list()?
            .into_iter()
            .filter(|f| has_words(&f.text, &words))
            .collect())
    }

    pub fn remove(&self, id: i64) -> Result<bool, DbError> {
        self.db
            .with(|c| c.execute("DELETE FROM facts WHERE id = ?1", [id]))
            .map(|n| n > 0)
    }

    pub fn clear(&self) -> Result<usize, DbError> {
        self.db.with(|c| c.execute("DELETE FROM facts", []))
    }

    fn set_vector(&self, id: i64, model: &str, v: &[f32]) -> Result<(), DbError> {
        self.db.with(|c| {
            c.execute(
                "UPDATE facts SET embedding = ?2, embedding_model = ?3 WHERE id = ?1",
                params![id, encode_vector(v), model],
            )
            .map(|_| ())
        })
    }

    /// Vectors made by `model`, by fact id.
    fn vectors(&self, model: &str) -> Result<HashMap<i64, Vec<f32>>, DbError> {
        self.db.with(|c| {
            let mut st = c.prepare(
                "SELECT id, embedding FROM facts WHERE embedding_model = ?1 AND embedding IS NOT NULL",
            )?;
            let rows = st.query_map([model], |r| {
                Ok((r.get::<_, i64>(0)?, decode_vector(&r.get::<_, Vec<u8>>(1)?)))
            })?;
            rows.collect()
        })
    }
}

fn block(facts: &[&Fact]) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    let lines: Vec<String> = facts.iter().map(|f| format!("- {}", f.text)).collect();
    Some(format!(
        "Что ты помнишь о пользователе (используй, когда уместно, не перечисляй без просьбы):\n{}",
        lines.join("\n")
    ))
}

/// Facts in the given order that fit the budget; at least one.
fn fit<'a>(ordered: impl Iterator<Item = &'a Fact>, budget: usize) -> Vec<&'a Fact> {
    let mut size = 0;
    let mut out = Vec::new();
    for f in ordered {
        size += f.text.chars().count() + 3;
        if size > budget && !out.is_empty() {
            break;
        }
        out.push(f);
    }
    out
}

/// One found piece of a past conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryHit {
    pub at: String,
    pub role: Role,
    pub text: String,
}

/// Facts plus past conversations, with an optional embedding model.
#[derive(Clone)]
pub struct Memory {
    pub facts: FactStore,
    db: Arc<Db>,
    embedder: Option<Arc<dyn Embedder>>,
    budget: usize,
}

impl Memory {
    pub fn new(db: Arc<Db>, embedder: Option<Arc<dyn Embedder>>) -> Self {
        Self {
            facts: FactStore::new(db.clone()),
            db,
            embedder,
            budget: FACTS_BUDGET,
        }
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget;
        self
    }

    /// The embedder and its model, when embeddings are on.
    fn active(&self) -> Option<(&Arc<dyn Embedder>, String)> {
        let e = self.embedder.as_ref()?;
        let model = e.model().filter(|m| !m.trim().is_empty())?;
        Some((e, model))
    }

    pub fn semantic(&self) -> bool {
        self.active().is_some()
    }

    /// Saves a fact and, when embeddings are on, its vector. A failed embedding
    /// keeps the fact: its vector is made later, on the first search.
    pub async fn save(&self, text: &str) -> Result<i64, DbError> {
        let id = self.facts.add(text)?;
        if let Some((e, model)) = self.active() {
            match e.embed(&[clip(text.trim())]).await {
                Ok(v) if v.len() == 1 => self.facts.set_vector(id, &model, &v[0])?,
                Ok(_) => log::warn!("embedding answer has the wrong size"),
                Err(err) => log::warn!("cannot embed fact: {err}"),
            }
        }
        Ok(id)
    }

    /// Makes the missing fact vectors and returns all of them.
    async fn fact_vectors(
        &self,
        e: &Arc<dyn Embedder>,
        model: &str,
        facts: &[Fact],
    ) -> Result<HashMap<i64, Vec<f32>>, String> {
        let mut have = self.facts.vectors(model).map_err(|e| e.to_string())?;
        let missing: Vec<&Fact> = facts.iter().filter(|f| !have.contains_key(&f.id)).collect();
        for chunk in missing.chunks(EMBED_BATCH) {
            let texts: Vec<String> = chunk.iter().map(|f| clip(&f.text)).collect();
            let vectors = e.embed(&texts).await?;
            if vectors.len() != chunk.len() {
                return Err("ответ модели эмбеддингов не той длины".into());
            }
            for (f, v) in chunk.iter().zip(vectors) {
                self.facts
                    .set_vector(f.id, model, &v)
                    .map_err(|e| e.to_string())?;
                have.insert(f.id, v);
            }
        }
        Ok(have)
    }

    /// Facts ranked by closeness to the query, the closest first.
    async fn ranked(&self, query: &str, facts: Vec<Fact>) -> Result<Vec<(f32, Fact)>, String> {
        let (e, model) = self.active().ok_or("эмбеддинги выключены")?;
        let q = e.embed(&[clip(query)]).await?;
        let q = q.first().ok_or("пустой ответ модели эмбеддингов")?;
        let vectors = self.fact_vectors(e, &model, &facts).await?;
        let mut scored: Vec<(f32, Fact)> = facts
            .into_iter()
            .map(|f| (vectors.get(&f.id).map_or(0.0, |v| cosine(q, v)), f))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(scored)
    }

    /// Facts for a query: by meaning when embeddings are on, else by words.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Fact>, DbError> {
        if query.trim().is_empty() {
            let mut all = self.facts.list()?;
            all.reverse();
            all.truncate(limit);
            return Ok(all);
        }
        if self.semantic() {
            match self.ranked(query, self.facts.list()?).await {
                Ok(scored) => return Ok(scored.into_iter().take(limit).map(|(_, f)| f).collect()),
                Err(e) => log::warn!("semantic fact search failed, using words: {e}"),
            }
        }
        let mut found = self.facts.search(query)?;
        found.truncate(limit);
        Ok(found)
    }

    /// Paragraph for the system prompt. Everything while it fits the budget;
    /// beyond that the facts closest to the message, or the newest without embeddings.
    pub async fn prompt_block(&self, user_text: &str) -> Option<String> {
        let facts = self.facts.list().unwrap_or_else(|e| {
            log::error!("cannot read facts: {e}");
            Vec::new()
        });
        let total: usize = facts.iter().map(|f| f.text.chars().count() + 3).sum();
        if total <= self.budget {
            return block(&facts.iter().collect::<Vec<_>>());
        }
        if self.semantic() {
            match tokio::time::timeout(RECALL_TIMEOUT, self.ranked(user_text, facts.clone())).await
            {
                Ok(Ok(scored)) => {
                    let ordered: Vec<Fact> = scored.into_iter().map(|(_, f)| f).collect();
                    return block(&fit(ordered.iter(), self.budget));
                }
                Ok(Err(e)) => log::warn!("semantic recall failed, using the newest facts: {e}"),
                Err(_) => log::warn!("semantic recall timed out, using the newest facts"),
            }
        }
        let mut newest = fit(facts.iter().rev(), self.budget);
        newest.reverse();
        block(&newest)
    }

    fn message_vectors(&self, model: &str) -> Result<HashMap<i64, Vec<f32>>, DbError> {
        self.db.with(|c| {
            let mut st =
                c.prepare("SELECT message_id, embedding FROM message_vectors WHERE model = ?1")?;
            let rows = st.query_map([model], |r| {
                Ok((r.get::<_, i64>(0)?, decode_vector(&r.get::<_, Vec<u8>>(1)?)))
            })?;
            rows.collect()
        })
    }

    async fn history_by_meaning(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HistoryHit>, String> {
        let (e, model) = self.active().ok_or("эмбеддинги выключены")?;
        let messages = self
            .db
            .recent_text_messages(HISTORY_WINDOW)
            .map_err(|e| e.to_string())?;
        let mut have = self.message_vectors(&model).map_err(|e| e.to_string())?;
        let missing: Vec<&(i64, String, Role, String)> = messages
            .iter()
            .filter(|m| !have.contains_key(&m.0))
            .take(EMBED_PER_SEARCH)
            .collect();
        for chunk in missing.chunks(EMBED_BATCH) {
            let texts: Vec<String> = chunk.iter().map(|m| clip(&m.3)).collect();
            let vectors = e.embed(&texts).await?;
            if vectors.len() != chunk.len() {
                return Err("ответ модели эмбеддингов не той длины".into());
            }
            for (m, v) in chunk.iter().zip(vectors) {
                self.db
                    .with(|c| {
                        c.execute(
                            "INSERT OR REPLACE INTO message_vectors (message_id, model, embedding) VALUES (?1, ?2, ?3)",
                            params![m.0, model, encode_vector(&v)],
                        )
                    })
                    .map_err(|e| e.to_string())?;
                have.insert(m.0, v);
            }
        }
        let q = e.embed(&[clip(query)]).await?;
        let q = q.first().ok_or("пустой ответ модели эмбеддингов")?;
        let mut scored: Vec<(f32, HistoryHit)> = messages
            .into_iter()
            .filter_map(|(id, at, role, text)| {
                let score = cosine(q, have.get(&id)?);
                Some((score, HistoryHit { at, role, text }))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(scored.into_iter().take(limit).map(|(_, h)| h).collect())
    }

    /// Pieces of past conversations for a query, from every conversation of the list.
    pub async fn search_history(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HistoryHit>, DbError> {
        if self.semantic() {
            match self.history_by_meaning(query, limit).await {
                Ok(hits) => return Ok(hits),
                Err(e) => log::warn!("semantic history search failed, using words: {e}"),
            }
        }
        Ok(self
            .db
            .search_messages(&words_of(query), limit)?
            .into_iter()
            .map(|(at, role, text)| HistoryHit { at, role, text })
            .collect())
    }

    /// Drops every stored vector, for "delete memory" and after a model change.
    pub fn clear_vectors(&self) -> Result<(), DbError> {
        self.db.with(|c| {
            c.execute("DELETE FROM message_vectors", [])?;
            c.execute(
                "UPDATE facts SET embedding = NULL, embedding_model = NULL",
                [],
            )
            .map(|_| ())
        })
    }
}

#[async_trait]
impl Recall for Memory {
    async fn recall(&self, user_text: &str) -> Option<String> {
        self.prompt_block(user_text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;
    use parking_lot::Mutex;

    /// Three axes: animals, drinks, cities. Counts calls and embedded texts.
    struct FakeEmbedder {
        on: bool,
        fail: bool,
        calls: Mutex<Vec<usize>>,
    }

    impl FakeEmbedder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                on: true,
                fail: false,
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    fn axis(text: &str) -> Vec<f32> {
        let t = text.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| t.contains(w)) as u8 as f32;
        vec![
            has(&["кот", "собак", "питом", "барсик"]),
            has(&["чай", "кофе", "напит"]),
            has(&["перм", "город", "живёт", "живет"]),
        ]
    }

    #[async_trait]
    impl Embedder for FakeEmbedder {
        fn model(&self) -> Option<String> {
            self.on.then(|| "fake-1".to_string())
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.calls.lock().push(texts.len());
            if self.fail {
                return Err("сеть".into());
            }
            Ok(texts.iter().map(|t| axis(t)).collect())
        }
    }

    fn db() -> Arc<Db> {
        Arc::new(Db::open_in_memory().unwrap())
    }

    fn plain() -> Memory {
        Memory::new(db(), None)
    }

    #[tokio::test]
    async fn facts_go_into_prompt_and_removed_ones_do_not() {
        let m = plain();
        assert_eq!(m.prompt_block("привет").await, None);
        let cat = m.save("Кота зовут Барсик").await.unwrap();
        m.save("Любит чай без сахара").await.unwrap();
        let p = m.prompt_block("привет").await.unwrap();
        assert!(p.contains("- Кота зовут Барсик\n- Любит чай без сахара"));
        assert!(m.facts.remove(cat).unwrap());
        assert!(!m.prompt_block("привет").await.unwrap().contains("Барсик"));
        assert!(!m.facts.remove(cat).unwrap());
    }

    #[tokio::test]
    async fn same_fact_is_stored_once() {
        let m = plain();
        let a = m.save("Живёт в Перми").await.unwrap();
        assert_eq!(m.save("  живёт в перми ").await.unwrap(), a);
        assert_eq!(m.facts.list().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn without_embeddings_a_small_budget_keeps_the_newest() {
        let m = plain().with_budget(20);
        m.save(&"а".repeat(50)).await.unwrap();
        m.save("новый факт").await.unwrap();
        let p = m.prompt_block("что угодно").await.unwrap();
        assert!(p.contains("новый факт") && !p.contains("ааа"));
    }

    #[tokio::test]
    async fn word_search_ignores_case_and_needs_all_words() {
        let m = plain();
        m.save("Кота зовут Барсик").await.unwrap();
        m.save("Собаку зовут Рекс").await.unwrap();
        assert_eq!(m.search("барсик кота", 5).await.unwrap().len(), 1);
        assert_eq!(m.search("зовут", 5).await.unwrap().len(), 2);
        assert!(m.search("попугай", 5).await.unwrap().is_empty());
        assert_eq!(m.search("", 1).await.unwrap()[0].text, "Собаку зовут Рекс");
    }

    #[tokio::test]
    async fn semantic_search_finds_a_fact_that_shares_no_word() {
        let e = FakeEmbedder::new();
        let m = Memory::new(db(), Some(e.clone()));
        m.save("Кота зовут Барсик").await.unwrap();
        m.save("Любит чай без сахара").await.unwrap();
        m.save("Живёт в Перми").await.unwrap();
        let found = m.search("какой у меня питомец", 1).await.unwrap();
        assert_eq!(found[0].text, "Кота зовут Барсик");
        // Fact vectors were made at save time: the search embedded only the query.
        assert_eq!(*e.calls.lock().last().unwrap(), 1);
    }

    #[tokio::test]
    async fn over_budget_the_prompt_takes_the_closest_facts() {
        let e = FakeEmbedder::new();
        let m = Memory::new(db(), Some(e)).with_budget(30);
        m.save("Кота зовут Барсик").await.unwrap();
        m.save("Любит чай без сахара").await.unwrap();
        m.save("Живёт в Перми").await.unwrap();
        let p = m.prompt_block("какой напиток мне сделать?").await.unwrap();
        assert!(p.contains("чай"), "{p}");
        assert!(!p.contains("Перми"), "{p}");
    }

    #[tokio::test]
    async fn facts_saved_without_embeddings_get_vectors_on_first_search() {
        let d = db();
        Memory::new(d.clone(), None)
            .save("Кота зовут Барсик")
            .await
            .unwrap();
        let e = FakeEmbedder::new();
        let m = Memory::new(d, Some(e.clone()));
        assert_eq!(m.search("питомец", 1).await.unwrap().len(), 1);
        assert_eq!(m.facts.vectors("fake-1").unwrap().len(), 1);
        m.clear_vectors().unwrap();
        assert!(m.facts.vectors("fake-1").unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_embedding_falls_back_to_words_and_keeps_the_fact() {
        let e = Arc::new(FakeEmbedder {
            on: true,
            fail: true,
            calls: Mutex::new(Vec::new()),
        });
        let m = Memory::new(db(), Some(e));
        m.save("Кота зовут Барсик").await.unwrap();
        assert_eq!(m.search("барсик", 5).await.unwrap().len(), 1);
        assert!(m.search("питомец", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn switched_off_embedder_is_not_called() {
        let e = Arc::new(FakeEmbedder {
            on: false,
            fail: false,
            calls: Mutex::new(Vec::new()),
        });
        let m = Memory::new(db(), Some(e.clone()));
        m.save("Кота зовут Барсик").await.unwrap();
        m.search("кот", 5).await.unwrap();
        assert!(!m.semantic());
        assert!(e.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn history_search_by_meaning_and_by_words() {
        let d = db();
        d.append_message("archive-1", &ChatMessage::user("Мой кот опять не ест"))
            .unwrap();
        d.append_message("main", &ChatMessage::assistant("Завари чай и отдохни"))
            .unwrap();
        d.append_message("main", &ChatMessage::tool_result("c1", "кот"))
            .unwrap();

        let words = Memory::new(d.clone(), None);
        let hits = words.search_history("кот", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, Role::User);

        let e = FakeEmbedder::new();
        let m = Memory::new(d.clone(), Some(e.clone()));
        let hits = m
            .search_history("что я говорил про питомца", 1)
            .await
            .unwrap();
        assert_eq!(hits[0].text, "Мой кот опять не ест");
        // Second search reuses the stored vectors: only the query is embedded.
        m.search_history("напиток", 1).await.unwrap();
        assert_eq!(*e.calls.lock().last().unwrap(), 1);

        // A deleted conversation is found neither by meaning nor by words,
        // and its vectors are gone with it (decision 0052).
        d.delete_conversation("archive-1").unwrap();
        let hits = m
            .search_history("что я говорил про питомца", 5)
            .await
            .unwrap();
        assert!(hits.iter().all(|h| h.text != "Мой кот опять не ест"));
        assert!(words.search_history("кот", 5).await.unwrap().is_empty());
        assert_eq!(m.message_vectors("fake-1").unwrap().len(), 1);
    }

    #[test]
    fn vector_codec_and_cosine() {
        let v = vec![0.5f32, -1.25, 3.0];
        assert_eq!(decode_vector(&encode_vector(&v)), v);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
    }
}
