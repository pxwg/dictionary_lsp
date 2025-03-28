use once_cell::sync::Lazy;
use rusqlite;
use std::sync::RwLock;
use std::time::Instant;
use tower_lsp::jsonrpc::Error;
use trie_rs::{Trie, TrieBuilder};

// Global trie instance, lazily initialized
static GLOBAL_TRIE: Lazy<RwLock<Option<Trie<char>>>> = Lazy::new(|| RwLock::new(None));
static LAST_INIT_TIME: Lazy<RwLock<Option<Instant>>> = Lazy::new(|| RwLock::new(None));

/// Initialize the global trie from a frequency database
pub fn initialize_global_trie(freq_path: &str) -> Result<(), Error> {
  // Check if we already initialized recently (avoid repeated initializations)
  if let Some(last_time) = *LAST_INIT_TIME.read().unwrap() {
    if last_time.elapsed().as_secs() < 3600 {
      // Once per hour is enough
      return Ok(());
    }
  }

  let mut trie_builder = TrieBuilder::new();

  // Connect to the SQLite frequency database
  let conn = rusqlite::Connection::open(freq_path).map_err(|e| {
    eprintln!("Failed to open frequency database: {}", e);
    Error::internal_error()
  })?;

  // Query words from the database - ordered by frequency for better suggestions
  let mut stmt = conn
    .prepare("SELECT word FROM word_frequencies ORDER BY frequency DESC LIMIT 100000")
    .map_err(|e| {
      eprintln!("Failed to prepare SQL statement: {}", e);
      Error::internal_error()
    })?;

  let rows = stmt
    .query_map([], |row| row.get::<_, String>(0))
    .map_err(|e| {
      eprintln!("Failed to query words: {}", e);
      Error::internal_error()
    })?;

  let start_time = Instant::now();
  let mut word_count = 0;

  // Add each word to the trie
  for word_result in rows {
    if let Ok(word) = word_result {
      let chars: Vec<char> = word.chars().collect();
      trie_builder.push(&chars);
      word_count += 1;
    }
  }

  // Build the trie and store it globally
  let trie = trie_builder.build();
  let mut global_trie = GLOBAL_TRIE.write().unwrap();
  *global_trie = Some(trie);

  let mut last_time = LAST_INIT_TIME.write().unwrap();
  *last_time = Some(Instant::now());

  eprintln!(
    "Trie initialized with {} words in {:?}",
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
  if let Some(trie) = GLOBAL_TRIE.read().unwrap().as_ref() {
    let char_vec: Vec<char> = prefix.chars().collect();
    let mut results: Vec<String> = trie
      .predictive_search(&char_vec)
      .into_iter()
      .map(|chars: Vec<char>| chars.into_iter().collect::<String>())
      .collect();

    // Truncate results if they exceed the limit
    if results.len() > limit {
      results.truncate(limit);
    }

    results
  } else {
    Vec::new()
  }
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
