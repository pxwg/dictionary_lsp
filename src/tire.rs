use lru::LruCache;
use once_cell::sync::Lazy;
use rusqlite;
use std::num::NonZeroUsize;
use std::sync::RwLock;
use std::time::Instant;
use tower_lsp::jsonrpc::Error;
use trie_rs::{Trie, TrieBuilder};

// Global trie instances, split by frequency tiers
pub static HIGH_FREQ_TRIE: Lazy<RwLock<Option<Trie<char>>>> = Lazy::new(|| RwLock::new(None));
pub static MID_FREQ_TRIE: Lazy<RwLock<Option<Trie<char>>>> = Lazy::new(|| RwLock::new(None));
pub static LOW_FREQ_TRIE: Lazy<RwLock<Option<Trie<char>>>> = Lazy::new(|| RwLock::new(None));
static LAST_INIT_TIME: Lazy<RwLock<Option<Instant>>> = Lazy::new(|| RwLock::new(None));
pub static PREFIX_CACHE: Lazy<RwLock<LruCache<String, Vec<String>>>> =
  Lazy::new(|| RwLock::new(LruCache::new(NonZeroUsize::new(1000).unwrap())));

/// Initialize the global trie from a frequency database
pub fn initialize_global_trie(freq_path: &str) -> Result<(), Error> {
  // Check if we already initialized recently (avoid repeated initializations)
  if let Some(last_time) = *LAST_INIT_TIME.read().unwrap() {
    if last_time.elapsed().as_secs() < 3600 {
      // Once per hour is enough
      return Ok(());
    }
  }

  let mut high_builder = TrieBuilder::new();
  let mut mid_builder = TrieBuilder::new();
  let mut low_builder = TrieBuilder::new();

  // Connect to the SQLite frequency database
  let conn = rusqlite::Connection::open(freq_path).map_err(|e| {
    eprintln!("Failed to open frequency database: {}", e);
    Error::internal_error()
  })?;

  // Query words from the database with their frequencies
  let mut stmt = conn
    .prepare("SELECT word, frequency FROM word_frequencies ORDER BY frequency DESC")
    .map_err(|e| {
      eprintln!("Failed to prepare SQL statement: {}", e);
      Error::internal_error()
    })?;

  let rows = stmt
    .query_map([], |row| {
      Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })
    .map_err(|e| {
      eprintln!("Failed to query words: {}", e);
      Error::internal_error()
    })?;

  let start_time = Instant::now();
  let mut word_count = 0;
  let mut high_count = 0;
  let mut mid_count = 0;
  let mut low_count = 0;

  // Collect rows into a Vec to get the count and make them indexable
  let rows: Vec<_> = rows.collect();
  let total_words = rows.len();
  let words_per_group = total_words / 3;

  // Add words to tries, distributing them evenly
  for (index, word_result) in rows.into_iter().enumerate() {
    if let Ok((word, _freq)) = word_result {
      let chars: Vec<char> = word.chars().collect();

      // Distribute words evenly across the three tries
      if index < words_per_group {
        // First third goes to high frequency
        high_builder.push(&chars);
        high_count += 1;
      } else if index < words_per_group * 2 {
        // Second third goes to medium frequency
        mid_builder.push(&chars);
        mid_count += 1;
      } else {
        // Final third goes to low frequency
        low_builder.push(&chars);
        low_count += 1;
      }

      word_count += 1;
    }
  }

  // Build the tries and store them globally
  let high_trie = high_builder.build();
  let mid_trie = mid_builder.build();
  let low_trie = low_builder.build();

  {
    let mut high_guard = HIGH_FREQ_TRIE.write().unwrap();
    *high_guard = Some(high_trie);
  }

  {
    let mut mid_guard = MID_FREQ_TRIE.write().unwrap();
    *mid_guard = Some(mid_trie);
  }

  {
    let mut low_guard = LOW_FREQ_TRIE.write().unwrap();
    *low_guard = Some(low_trie);
  }

  // Clear the cache when dictionary is reloaded
  {
    let mut cache = PREFIX_CACHE.write().unwrap();
    cache.clear();
  }

  let mut last_time = LAST_INIT_TIME.write().unwrap();
  *last_time = Some(Instant::now());

  eprintln!(
    "Tries initialized with {} words (high: {}, mid: {}, low: {}) in {:?}",
    word_count,
    high_count,
    mid_count,
    low_count,
    start_time.elapsed()
  );

  Ok(())
}

/// Check if the trie is initialized
pub fn is_trie_initialized() -> bool {
  HIGH_FREQ_TRIE.read().unwrap().is_some()
}

/// Find words by prefix using the global trie
pub fn find_words_by_prefix(prefix: &str, limit: usize) -> Vec<String> {
  // Check cache with a read lock first (better concurrency)
  if let Some(cached_results) = PREFIX_CACHE
    .read()
    .unwrap()
    .clone()
    .get(&prefix.to_string())
  {
    return cached_results.clone();
  }

  let char_vec: Vec<char> = prefix.chars().collect();
  let mut results = Vec::with_capacity(limit); // Pre-allocate memory

  // Define the tries we'll search in priority order
  let tries = [
    &HIGH_FREQ_TRIE as &RwLock<Option<Trie<char>>>,
    &MID_FREQ_TRIE,
    &LOW_FREQ_TRIE,
  ];

  // Search each trie in order until we have enough results
  for trie_lock in tries {
    // Skip if we already have enough results
    if results.len() >= limit {
      break;
    }

    // Search the current trie
    if let Some(trie) = trie_lock.read().unwrap().as_ref() {
      let needed = limit - results.len();
      let mut matches = trie
        .predictive_search(&char_vec)
        .into_iter()
        .map(|chars: Vec<char>| chars.into_iter().collect::<String>())
        .take(needed) // Only take what we need
        .collect::<Vec<String>>();

      results.append(&mut matches);
    }
  }

  // Cache the results
  let mut cache = PREFIX_CACHE.write().unwrap();
  cache.put(prefix.to_string(), results.clone());

  results
}

/// Handles case sensitivity for prefix searches
pub fn find_words_respecting_case(prefix: &str, limit: usize) -> Vec<String> {
  // Get lowercase results
  let results = find_words_by_prefix(&prefix.to_lowercase(), limit);

  // Check if the original prefix starts with uppercase
  if prefix.chars().next().map_or(false, |c| c.is_uppercase()) {
    // Capitalize the first letter of each result
    results
      .into_iter()
      .map(|word| {
        let mut chars = word.chars();
        match chars.next() {
          None => String::new(),
          Some(first_char) => first_char.to_uppercase().collect::<String>() + chars.as_str(),
        }
      })
      .collect()
  } else {
    results
  }
}
