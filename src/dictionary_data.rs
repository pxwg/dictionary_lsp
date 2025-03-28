use crate::config::Config;
use crate::fuzzy;
use async_trait::async_trait;
use rusqlite;
use serde::{Deserialize, Serialize};
use serde_json;
use std::vec;
use tower_lsp::jsonrpc::Error;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::Position;

/// Determines if the character is a CJK (Chinese, Japanese, Korean) character
/// by checking if it falls within the Unicode ranges for CJK characters.
/// This helps properly handle word boundaries for Asian languages.
pub fn is_cjk_char(c: char) -> bool {
  (c >= '\u{4E00}' && c <= '\u{9FFF}')  // CJK Unified Ideographs
        || (c >= '\u{3400}' && c <= '\u{4DBF}')  // CJK Unified Ideographs Extension A
        || (c >= '\u{20000}' && c <= '\u{2A6DF}')  // CJK Unified Ideographs Extension B
        || (c >= '\u{2A700}' && c <= '\u{2B73F}')  // CJK Unified Ideographs Extension C
        || (c >= '\u{2B740}' && c <= '\u{2B81F}') // CJK Unified Ideographs Extension D
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DictionaryResponse {
  pub word: String,
  pub meanings: Vec<Meaning>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Meaning {
  pub part_of_speech: String,
  pub definitions: Vec<Definition>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Definition {
  pub definition: String,
  pub example: Option<String>,
}

/// Common trait for dictionary data providers
#[async_trait]
pub trait DictionaryProvider: Send + Sync {
  async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>>;
  fn get_word_at_position(&self, content: &str, position: Position) -> Option<String>;
  async fn find_words_by_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>>;
}

/// Factory function to create the appropriate dictionary provider
pub fn create_dictionary_provider(
  dictionary_path: Option<String>,
  freq_path: Option<String>,
) -> Box<dyn DictionaryProvider> {
  if Config::is_sqlite(dictionary_path.as_deref()) {
    Box::new(SqliteDictionaryProvider::new(dictionary_path, freq_path))
  } else {
    Box::new(JsonDictionaryProvider::new(dictionary_path, freq_path))
  }
}

/// Provider implementation for SQLite dictionaries
pub struct SqliteDictionaryProvider {
  dictionary_path: Option<String>,
  freq_path: Option<String>,
  dictionary_conn: tokio::sync::Mutex<Option<rusqlite::Connection>>,
  freq_conn: tokio::sync::Mutex<Option<rusqlite::Connection>>,
  prefix_cache: tokio::sync::Mutex<(String, Vec<String>)>,
}

impl SqliteDictionaryProvider {
  pub fn new(dictionary_path: Option<String>, freq_path: Option<String>) -> Self {
    let provider = Self {
      dictionary_path,
      freq_path,
      dictionary_conn: tokio::sync::Mutex::new(None),
      freq_conn: tokio::sync::Mutex::new(None),
      prefix_cache: tokio::sync::Mutex::new((String::new(), Vec::new())),
    };

    // Eagerly initialize connections if paths are available
    if let Some(dict_path) = &provider.dictionary_path {
      if let Ok(conn) = rusqlite::Connection::open(dict_path) {
        let _ = std::mem::replace(
          &mut *futures::executor::block_on(provider.dictionary_conn.lock()),
          Some(conn),
        );
      }
    }

    if let Some(freq_path) = &provider.freq_path {
      if let Ok(conn) = rusqlite::Connection::open(freq_path) {
        let _ = std::mem::replace(
          &mut *futures::executor::block_on(provider.freq_conn.lock()),
          Some(conn),
        );
      }
    }

    provider
  }
  fn get_dictionary_path(&self) -> Result<String> {
    match &self.dictionary_path {
      Some(path) => Ok(path.clone()),
      None => Err(Error::invalid_params("Dictionary path not provided")),
    }
  }
  fn get_freq_path(&self) -> Result<String> {
    match &self.freq_path {
      Some(path) => Ok(path.clone()),
      None => Err(Error::invalid_params("Frequency path not provided")),
    }
  }
  fn get_safe_string(row: &rusqlite::Row, idx: usize) -> Option<String> {
    match row.get::<_, Option<String>>(idx) {
      Ok(Some(s)) => Some(s),
      _ => match row.get::<_, Option<Vec<u8>>>(idx) {
        Ok(Some(bytes)) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        _ => None,
      },
    }
  }

  fn find_exact_match(
    &self,
    conn: &rusqlite::Connection,
    word: &str,
  ) -> Result<Option<DictionaryResponse>> {
    let mut stmt = conn
      .prepare(
        r#"
        SELECT 
            w.word,
            p.name AS pos,
            d.definition
        FROM words w
        JOIN definitions d ON w.id = d.word_id
        JOIN parts_of_speech p ON d.pos_id = p.id
        WHERE w.word = ?1 COLLATE NOCASE
        ORDER BY p.name
        "#,
      )
      .map_err(|e| {
        eprintln!("Error preparing statement: {}", e);
        Error::internal_error()
      })?;

    let query_result = stmt.query_map([word], |row| {
      let word = Self::get_safe_string(row, 0).unwrap_or_default();
      let pos = Self::get_safe_string(row, 1);
      let translation = Self::get_safe_string(row, 2);

      Ok((word, translation, pos))
    });

    match query_result {
      Ok(mut rows) => {
        if let Some(row_result) = rows.next() {
          match row_result {
            Ok((word, translation, pos)) => {
              let mut definitions = Vec::new();
              if let Some(trans) = translation {
                definitions.push(Definition {
                  definition: trans,
                  example: None,
                });
              }
              if definitions.is_empty() {
                return Ok(None);
              }
              return Ok(Some(DictionaryResponse {
                word,
                meanings: vec![Meaning {
                  part_of_speech: pos.unwrap_or_else(|| "unknown".to_string()),
                  definitions,
                }],
              }));
            }
            Err(e) => {
              eprintln!("Error processing row: {}", e);
              return Err(Error::internal_error());
            }
          }
        }
        Ok(None)
      }
      Err(e) => {
        eprintln!("Error querying database: {}", e);
        Err(Error::internal_error())
      }
    }
  }

  // Function to enable benchmarking with controllable distance parameter
  pub async fn find_words_by_prefix_with_distance(
    &self,
    prefix: &str,
    include_distance_2: bool,
  ) -> Result<Option<Vec<String>>> {
    // Convert prefix to lowercase for case-insensitive search
    let lowercase_prefix = prefix.to_lowercase();

    // Generate candidate words with controllable distance parameter
    let candidate_words =
      fuzzy::generate_levenshtein_candidates(&lowercase_prefix, include_distance_2).await;

    Ok(Some(candidate_words))
  }

  fn find_fuzzy_match(
    &self,
    conn: &rusqlite::Connection,
    word: &str,
  ) -> Result<Option<DictionaryResponse>> {
    let word_len = word.len() as i64;
    let max_distance = 2;
    let mut stmt = match conn.prepare(
      r#"
        SELECT 
            w.word,
            p.name AS pos,
            d.definition
        FROM words w
        JOIN definitions d ON w.id = d.word_id
        JOIN parts_of_speech p ON d.pos_id = p.id
        WHERE length(w.word) BETWEEN ?1 - ?2 AND ?1 + ?2
          AND substr(w.word, 1, 1) = substr(?3, 1, 1)
          AND substr(w.word, -1, 1) = substr(?3, -1, 1)
        ORDER BY length(w.word)
        "#,
    ) {
      Ok(stmt) => stmt,
      Err(e) => {
        eprintln!("Error preparing statement: {}", e);
        return Err(Error::internal_error());
      }
    };

    let query_result = stmt.query_map(rusqlite::params![word_len, max_distance, word], |row| {
      let word = Self::get_safe_string(row, 0).unwrap_or_default();
      let pos = Self::get_safe_string(row, 1);
      let translation = Self::get_safe_string(row, 2);
      let detail = Self::get_safe_string(row, 3);

      Ok((word, translation, pos, detail))
    });

    let max_distance = 2;
    let mut closest_match = None;
    let mut min_distance = max_distance + 1;

    match query_result {
      Ok(rows) => {
        for row_result in rows {
          match row_result {
            Ok((dict_word, translation, pos, detail)) => {
              let distance = self.levenshtein_distance(word, &dict_word);
              if distance <= max_distance && distance < min_distance {
                min_distance = distance;
                closest_match = Some((dict_word, translation, pos, detail));
              }
            }
            Err(e) => {
              eprintln!("Error processing row: {}", e);
              return Err(Error::internal_error());
            }
          }
        }

        if let Some((word, translation, pos, detail)) = closest_match {
          return Ok(Some(self.parse_dictionary_entry(
            &word,
            translation,
            pos,
            detail,
          )));
        }

        Ok(None)
      }
      Err(e) => {
        eprintln!("Error querying database: {}", e);
        Err(Error::internal_error())
      }
    }
  }

  fn parse_dictionary_entry(
    &self,
    word: &str,
    translation: Option<String>,
    pos: Option<String>,
    detail: Option<String>,
  ) -> DictionaryResponse {
    let mut definitions = Vec::new();

    if let Some(trans) = translation {
      definitions.push(Definition {
        definition: trans,
        example: None,
      });
    }

    if let Some(det) = detail {
      if !definitions.iter().any(|d| d.definition == det) {
        definitions.push(Definition {
          definition: det,
          example: None,
        });
      }
    }

    DictionaryResponse {
      word: word.to_string(),
      meanings: vec![Meaning {
        part_of_speech: pos.unwrap_or_else(|| "unknown".to_string()),
        definitions,
      }],
    }
  }
  // Calculate Levenshtein distance between two strings
  fn levenshtein_distance(&self, s1: &str, s2: &str) -> usize {
    let len1 = s1.chars().count();
    let len2 = s2.chars().count();
    if len1 == 0 {
      return len2;
    }
    if len2 == 0 {
      return len1;
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let mut matrix = vec![vec![0; len2 + 1]; len1 + 1];

    for i in 0..=len1 {
      matrix[i][0] = i;
    }
    for j in 0..=len2 {
      matrix[0][j] = j;
    }

    // Fill the matrix
    for i in 1..=len1 {
      for j in 1..=len2 {
        let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
          0
        } else {
          1
        };
        matrix[i][j] = std::cmp::min(
          std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
          matrix[i - 1][j - 1] + cost,
        );
      }
    }

    matrix[len1][len2]
  }
}

#[async_trait]
impl DictionaryProvider for SqliteDictionaryProvider {
  async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>> {
    let word_lower = word;

    let mut conn_guard = self.dictionary_conn.lock().await;
    if conn_guard.is_none() {
      let dict_path = self.get_dictionary_path()?;
      *conn_guard = Some(rusqlite::Connection::open(&dict_path).map_err(|e| {
        eprintln!("error connecting to sqlite database: {}", e);
        Error::internal_error()
      })?);
    }
    let conn = conn_guard.as_ref().unwrap();

    if let Some(response) = self.find_exact_match(&conn, &word_lower)? {
      return Ok(Some(response));
    }

    if let Some(response) = self.find_fuzzy_match(&conn, &word_lower)? {
      return Ok(Some(response));
    }

    // no matches found
    Ok(None)
  }

  fn get_word_at_position(&self, content: &str, position: Position) -> Option<String> {
    extract_word_at_position(content, position)
  }

  /// TODO: Add incremental search
  /// Add thread pool
  ///
  /// Find words by prefix
  async fn find_words_by_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
    // Check if the prefix is empty
    if prefix.is_empty() {
      return Ok(None);
    }

    // Convert prefix to lowercase for case-insensitive search
    let lowercase_prefix = prefix.to_lowercase();

    // Check if we can use cached results
    let mut cache = self.prefix_cache.lock().await;
    let (cached_prefix, cached_results) = &*cache;

    // If the new prefix extends the cached prefix, filter the cached results
    if !cached_prefix.is_empty()
      && lowercase_prefix.starts_with(cached_prefix)
      && !cached_results.is_empty()
      && lowercase_prefix != *cached_prefix
    {
      // Filter cached results that match the new prefix
      let filtered: Vec<String> = cached_results
        .iter()
        .filter(|word| word.to_lowercase().starts_with(&lowercase_prefix))
        .cloned()
        .collect();

      // If we found matches, update cache and return
      if !filtered.is_empty() {
        *cache = (lowercase_prefix.clone(), filtered.clone());
        return Ok(Some(filtered));
      }
    }

    // Try to use the global trie first
    if crate::tire::is_trie_initialized() {
      let results = crate::tire::find_words_by_prefix(&lowercase_prefix, 5);

      // If we got results from the global trie, update cache and return
      if !results.is_empty() {
        *cache = (lowercase_prefix, results.clone());
        return Ok(Some(results));
      }
    }

    // Fallback to fuzzy search if trie doesn't have results
    let mut candidate_words = fuzzy::generate_levenshtein_candidates(&lowercase_prefix, true).await;

    candidate_words.push(lowercase_prefix.clone());

    const MAX_CANDIDATES: usize = 100;
    if candidate_words.len() > MAX_CANDIDATES {
      candidate_words.truncate(MAX_CANDIDATES);
    }

    if candidate_words.is_empty() {
      *cache = (String::new(), Vec::new()); // Clear cache on failure
      return Ok(None);
    }

    let freq_path = self.get_freq_path()?;

    // Process all candidates in one go since our generation is now more targeted
    let batch_results = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
      let conn = rusqlite::Connection::open(&freq_path).map_err(|_e| Error::internal_error())?;
      let placeholders = vec!["?"; candidate_words.len()].join(",");

      // Query with proper result limit
      let query = format!(
        "SELECT word FROM word_frequencies WHERE word IN ({}) ORDER BY frequency DESC LIMIT 5",
        placeholders
      );

      let mut stmt = conn.prepare(&query).map_err(|_| Error::internal_error())?;

      // Convert words to SQL parameters
      let params: Vec<&dyn rusqlite::types::ToSql> = candidate_words
        .iter()
        .map(|w| w as &dyn rusqlite::types::ToSql)
        .collect();

      // Execute query and collect results
      let mut result = Vec::new();
      let rows = stmt
        .query_map(params.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|_| Error::internal_error())?;

      for word_result in rows {
        if let Ok(word) = word_result {
          result.push(word);
        }
      }

      Ok(result)
    })
    .await
    .map_err(|_| Error::internal_error())??;

    // Update cache with new results
    if !batch_results.is_empty() {
      *cache = (lowercase_prefix, batch_results.clone());
      Ok(Some(batch_results))
    } else {
      *cache = (String::new(), Vec::new());
      Ok(None)
    }
  }
}

/// Provider implementation for JSON dictionaries
pub struct JsonDictionaryProvider {
  dictionary_path: Option<String>,
  freq_path: Option<String>,
  dictionary_cache: tokio::sync::Mutex<Option<serde_json::Value>>,
  prefix_cache: tokio::sync::Mutex<(String, Vec<String>)>,
}

impl JsonDictionaryProvider {
  pub fn new(dictionary_path: Option<String>, freq_path: Option<String>) -> Self {
    let provider = Self {
      dictionary_path,
      freq_path,
      dictionary_cache: tokio::sync::Mutex::new(None),
      prefix_cache: tokio::sync::Mutex::new((String::new(), Vec::new())),
    };

    // Eagerly load dictionary if path is available
    if let Some(dict_path) = &provider.dictionary_path {
      if let Ok(contents) = std::fs::read_to_string(dict_path) {
        if let Ok(dict) = serde_json::from_str(&contents) {
          let _ = std::mem::replace(
            &mut *futures::executor::block_on(provider.dictionary_cache.lock()),
            Some(dict),
          );
        }
      }
    }

    provider
  }

  fn get_dictionary_path(&self) -> Result<String> {
    match &self.dictionary_path {
      Some(path) => Ok(path.clone()),
      None => Err(Error::invalid_params("Dictionary path not provided")),
    }
  }

  fn read_dictionary_file(&self, dict_path: &str) -> Result<serde_json::Value> {
    match std::fs::read_to_string(dict_path) {
      Ok(contents) => match serde_json::from_str(&contents) {
        Ok(dict) => Ok(dict),
        Err(e) => {
          eprintln!("Error parsing dictionary JSON: {}", e);
          Err(Error::internal_error())
        }
      },
      Err(e) => {
        eprintln!("Error reading dictionary file: {}", e);
        Err(Error::internal_error())
      }
    }
  }

  fn find_exact_match(
    &self,
    dictionary: &serde_json::Value,
    word: &str,
  ) -> Option<DictionaryResponse> {
    dictionary
      .get(word)
      .map(|entry| self.parse_dictionary_entry(word, entry, Some(word)))
  }

  fn parse_dictionary_entry(
    &self,
    word: &str,
    entry: &serde_json::Value,
    _original_query: Option<&str>,
  ) -> DictionaryResponse {
    let mut meanings = Vec::new();

    if let Some(obj) = entry.as_object() {
      for (part_of_speech, defs) in obj {
        if let Some(defs_array) = defs.as_array() {
          let definitions = defs_array
            .iter()
            .map(|def| Definition {
              definition: def.as_str().unwrap_or("").to_string(),
              example: None,
            })
            .collect();

          meanings.push(Meaning {
            part_of_speech: part_of_speech.clone(),
            definitions,
          });
        }
      }
    }

    DictionaryResponse {
      word: word.to_string(),
      meanings,
    }
  }

  fn find_fuzzy_match(
    &self,
    dictionary: &serde_json::Value,
    word: &str,
  ) -> Option<DictionaryResponse> {
    let max_distance = 2;
    let mut closest_match = None;
    let mut min_distance = max_distance + 1;

    // Find the closest match within our threshold
    if let Some(entries) = dictionary.as_object() {
      for (dict_word, entry) in entries {
        let distance = self.levenshtein_distance(word, dict_word);
        if distance <= max_distance && distance < min_distance {
          min_distance = distance;
          closest_match = Some((dict_word.clone(), entry));
        }
      }
    }

    closest_match
      .map(|(matched_word, entry)| self.parse_dictionary_entry(&matched_word, entry, Some(word)))
  }

  // Calculate Levenshtein distance between two strings
  fn levenshtein_distance(&self, s1: &str, s2: &str) -> usize {
    let len1 = s1.chars().count();
    let len2 = s2.chars().count();
    if len1 == 0 {
      return len2;
    }
    if len2 == 0 {
      return len1;
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let mut matrix = vec![vec![0; len2 + 1]; len1 + 1];

    for i in 0..=len1 {
      matrix[i][0] = i;
    }
    for j in 0..=len2 {
      matrix[0][j] = j;
    }

    // Fill the matrix
    for i in 1..=len1 {
      for j in 1..=len2 {
        let cost = if s1_chars[i - 1] == s2_chars[j - 1] {
          0
        } else {
          1
        };
        matrix[i][j] = std::cmp::min(
          std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
          matrix[i - 1][j - 1] + cost,
        );
      }
    }

    matrix[len1][len2]
  }
}

#[async_trait]
impl DictionaryProvider for JsonDictionaryProvider {
  async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>> {
    let word_lower = word.to_lowercase();
    let dictionary = match &*self.dictionary_cache.lock().await {
      Some(dict) => dict.clone(),
      None => {
        let dict_path = self.get_dictionary_path()?;
        let dict = self.read_dictionary_file(&dict_path)?;
        let mut cache = self.dictionary_cache.lock().await;
        *cache = Some(dict.clone());
        dict
      }
    };

    if let Some(response) = self.find_exact_match(&dictionary, &word_lower) {
      return Ok(Some(response));
    }

    if let Some(response) = self.find_fuzzy_match(&dictionary, &word_lower) {
      return Ok(Some(response));
    }

    Ok(None)
  }

  fn get_word_at_position(&self, content: &str, position: Position) -> Option<String> {
    extract_word_at_position(content, position)
  }

  async fn find_words_by_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
    // Always generate completions, even for single characters
    if prefix.is_empty() {
      return Ok(None);
    }

    // Check if we can use cached results
    let mut cache = self.prefix_cache.lock().await;
    let (cached_prefix, cached_results) = &*cache;

    // If the new prefix extends the cached prefix, filter the cached results
    if !cached_prefix.is_empty()
      && prefix.to_lowercase().starts_with(cached_prefix)
      && !cached_results.is_empty()
    {
      // Filter cached results that match the new prefix
      let filtered: Vec<String> = cached_results
        .iter()
        .filter(|word| word.to_lowercase().starts_with(&prefix.to_lowercase()))
        .cloned()
        .collect();

      // If we found matches, update cache and return
      if !filtered.is_empty() {
        *cache = (prefix.to_lowercase(), filtered.clone());
        return Ok(Some(filtered));
      }
    }

    let dictionary = match &*self.dictionary_cache.lock().await {
      Some(dict) => dict.clone(),
      None => {
        let dict_path = self.get_dictionary_path()?;
        let dict = self.read_dictionary_file(&dict_path)?;
        let mut cache = self.dictionary_cache.lock().await;
        *cache = Some(dict.clone());
        dict
      }
    };

    if let Some(entries) = dictionary.as_object() {
      let prefix_lower = prefix.to_lowercase();
      // Collect matching words, taking up to 100 for single character inputs
      let limit = if prefix.len() <= 1 { 100 } else { 50 };
      let matching_words: Vec<String> = entries
        .keys()
        .filter(|word| word.to_lowercase().starts_with(&prefix_lower))
        .take(limit)
        .map(|word| word.clone())
        .collect();

      if !matching_words.is_empty() {
        *cache = (prefix_lower, matching_words.clone());
        return Ok(Some(matching_words));
      }
    }

    // If no direct matches are found, use fuzzy matching
    let candidates = fuzzy::generate_levenshtein_candidates(prefix, true).await;
    if candidates.is_empty() {
      *cache = (String::new(), Vec::new()); // Clear cache on failure
      Ok(None)
    } else {
      *cache = (prefix.to_lowercase(), candidates.clone());
      Ok(Some(candidates))
    }
  }
}

/// Common function to extract a word at a given position in text
pub fn extract_word_at_position(
  content: &str,
  position: tower_lsp::lsp_types::Position,
) -> Option<String> {
  let lines: Vec<&str> = content.lines().collect();
  if position.line as usize >= lines.len() {
    return None;
  }

  let line = lines[position.line as usize];
  let chars: Vec<char> = line.chars().collect();
  let char_pos = position.character as usize;

  if char_pos >= chars.len() {
    return None;
  }

  let mut start = char_pos;
  let mut end = char_pos;

  while start > 0 && (chars[start - 1].is_alphabetic() || is_cjk_char(chars[start - 1])) {
    start -= 1;
  }

  while end < chars.len() && (chars[end].is_alphabetic() || is_cjk_char(chars[end])) {
    end += 1;
  }

  if start == end {
    None
  } else {
    Some(chars[start..end].iter().collect())
  }
}
