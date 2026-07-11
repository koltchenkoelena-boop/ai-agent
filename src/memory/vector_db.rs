// ---------------------------------------------------------------------------
// Long-Term Vector Memory — embedded RAG слой на базе sled (KV-движок)
//
//   VectorMemoryStore  → persistентное хранилище эмбеддингов
//   VectorEntry        → запись: текст + вектор + таймстемп
//   cosine_similarity  → ручной расчёт косинусного сходства
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use sled::Db;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// VectorEntry
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VectorEntry {
    pub text: String,
    pub embedding: Vec<f32>,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Cosine similarity
// ---------------------------------------------------------------------------

/// Вычислить косинусное сходство между двумя векторами.
///
/// Возвращает значение в диапазоне [-1.0, 1.0].
/// Паникает, если векторы разной длины (caller должен гарантировать).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vectors must have the same length");

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

// ---------------------------------------------------------------------------
// VectorMemoryStore
// ---------------------------------------------------------------------------

/// Embedded векторное хранилище на базе sled (persistent KV).
///
/// Ключ: UUID (v4) → значение: bincode-serialized VectorEntry.
pub struct VectorMemoryStore {
    db: Db,
}

impl VectorMemoryStore {
    /// Открыть (или создать) базу данных по пути `path`.
    ///
    /// Например: `sled::open("agent_memory.db")`.
    pub fn open(path: &str) -> Result<Self, String> {
        let db = sled::open(path).map_err(|e| format!("Failed to open sled db: {e}"))?;
        Ok(Self { db })
    }

    /// Сохранить запись в базу данных.
    ///
    /// Сериализует `entry` через bincode и сохраняет под уникальным UUID-ключом.
    pub fn insert(&self, entry: &VectorEntry) -> Result<(), String> {
        let key = Uuid::new_v4();
        let value =
            bincode::serialize(entry).map_err(|e| format!("Failed to serialize entry: {e}"))?;
        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| format!("Failed to insert into sled: {e}"))?;
        Ok(())
    }

    /// Поиск top_k записей по косинусному сходству с query_vector.
    ///
    /// Сканирует всю базу (линейный поиск, O(n) по числу записей).
    /// Возвращает до `top_k` результатов, отсортированных по убыванию сходства.
    pub fn query(&self, query_vector: &[f32], top_k: usize) -> Vec<VectorEntry> {
        let mut scored: Vec<(f32, VectorEntry)> = Vec::new();

        for result in self.db.iter() {
            let (_key, value) = match result {
                Ok(kv) => kv,
                Err(_) => continue,
            };

            let entry: VectorEntry = match bincode::deserialize(&value) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let sim = cosine_similarity(query_vector, &entry.embedding);
            scored.push((sim, entry));
        }

        // Сортировка по убыванию сходства
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        scored.into_iter().take(top_k).map(|(_, entry)| entry).collect()
    }

    /// Количество записей в хранилище.
    pub fn len(&self) -> usize {
        self.db.len()
    }

    pub fn is_empty(&self) -> bool {
        self.db.is_empty()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Временное хранилище в памяти (temp dir).
    fn temp_store() -> VectorMemoryStore {
        let dir = std::env::temp_dir().join(format!("test_vector_db_{}", Uuid::new_v4()));
        // Удаляем при падении теста тоже
        let _ = std::fs::remove_dir_all(&dir);
        VectorMemoryStore::open(dir.to_str().unwrap()).unwrap()
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        // Косинус идентичного вектора = 1.0 (с учётом float imprecision)
        assert!((sim - 1.0).abs() < 1e-6, "Expected ~1.0, got {sim}");
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6, "Expected ~0.0, got {sim}");
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 2.0];
        let b = vec![-1.0, -2.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6, "Expected ~-1.0, got {sim}");
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 2.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6, "Expected 0.0 for zero vector, got {sim}");
    }

    #[test]
    fn test_insert_and_query() {
        let store = temp_store();

        let e1 = VectorEntry {
            text: "The capital of France is Paris".into(),
            embedding: vec![1.0, 0.0, 0.0],
            timestamp: 100,
        };
        let e2 = VectorEntry {
            text: "Python is a programming language".into(),
            embedding: vec![0.0, 1.0, 0.0],
            timestamp: 200,
        };

        store.insert(&e1).unwrap();
        store.insert(&e2).unwrap();

        assert_eq!(store.len(), 2);

        // Запрос, похожий на e1
        let query = vec![0.95, 0.1, 0.0];
        let results = store.query(&query, 1);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].text.contains("Paris"),
            "Expected Paris entry first, got: {}",
            results[0].text
        );
    }

    #[test]
    fn test_query_top_k() {
        let store = temp_store();

        // Вставляем три вектора: все одинаковые (макс сходство)
        for i in 0..3 {
            let entry = VectorEntry {
                text: format!("Entry {i}"),
                embedding: vec![1.0, 0.0],
                timestamp: i as u64,
            };
            store.insert(&entry).unwrap();
        }

        let query = vec![1.0, 0.0];
        let results = store.query(&query, 2);
        assert_eq!(results.len(), 2, "top_k=2 should return 2 results");
    }

    #[test]
    fn test_empty_store() {
        let store = temp_store();
        assert!(store.is_empty());

        let query = vec![0.1, 0.2];
        let results = store.query(&query, 5);
        assert!(results.is_empty(), "Empty store should return no results");
    }
}
