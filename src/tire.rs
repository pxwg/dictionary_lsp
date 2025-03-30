use lru::LruCache;
use once_cell::sync::Lazy;
use rusqlite;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::RwLock;
use std::time::Instant;
use tower_lsp::jsonrpc::Error;
use trie_rs::{Trie, TrieBuilder};

// Global trie instances, split by frequency tiers
pub static WORD_FREQUENCIES: Lazy<RwLock<HashMap<String, i64>>> =
  Lazy::new(|| RwLock::new(HashMap::new()));
pub static GLOBAL_TRIE: Lazy<RwLock<Option<Trie<char>>>> = Lazy::new(|| RwLock::new(None));
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
  let mut builder = TrieBuilder::new();

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

  // Add all words to the trie in frequency order (already sorted by SQL query)
  for word_result in rows {
    let mut freq_map = WORD_FREQUENCIES.write().unwrap();
    if let Ok((word, freq)) = word_result {
      let chars: Vec<char> = word.chars().collect();
      builder.push(&chars);
      freq_map.insert(word, freq);
      word_count += 1;
    }
  }

  // Build the trie and store it globally
  let trie = builder.build();

  {
    let mut trie_guard = GLOBAL_TRIE.write().unwrap();
    *trie_guard = Some(trie);
  }

  // Clear the cache when dictionary is reloaded
  {
    let mut cache = PREFIX_CACHE.write().unwrap();
    cache.clear();
  }

  let mut last_time = LAST_INIT_TIME.write().unwrap();
  *last_time = Some(Instant::now());

  eprintln!(
    "Trie is initialized with {} words in {:?}",
    word_count,
    start_time.elapsed()
  );

  Ok(())
}

/// Check if the trie is initialized
pub fn is_trie_initialized() -> bool {
  GLOBAL_TRIE.read().unwrap().is_some()
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
  let tries = [&GLOBAL_TRIE as &RwLock<Option<Trie<char>>>];

  // Search each trie in order until we have enough results
  for trie_lock in tries {
    // Skip if we already have enough results
    if results.len() >= limit {
      break;
    }

    // Search the current trie
    if let Some(trie) = trie_lock.read().unwrap().as_ref() {
      let matches = trie
        .predictive_search(&char_vec)
        .into_iter()
        .map(|chars: Vec<char>| chars.into_iter().collect::<String>())
        // .take(needed) // Only take what we need
        .collect::<Vec<String>>();

      let freq_map = WORD_FREQUENCIES.read().unwrap();
      let mut sorted_matches = matches;
      sorted_matches.sort_by(|a, b| {
        freq_map
          .get(b)
          .unwrap_or(&0)
          .cmp(freq_map.get(a).unwrap_or(&0))
      });

      results.extend(sorted_matches.into_iter().take(limit - results.len()));
    }
  }

  // Cache the results
  let mut cache = PREFIX_CACHE.write().unwrap();
  cache.put(prefix.to_string(), results.clone());

  results
}

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
          Some(first_char) => {
            let mut result = String::with_capacity(word.len());
            result.extend(first_char.to_uppercase());
            result.push_str(chars.as_str());
            result
          }
        }
      })
      .collect()
  } else {
    results
  }
}
