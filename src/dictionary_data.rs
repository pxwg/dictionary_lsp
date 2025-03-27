use std::vec;

use crate::config::Config;
use async_trait::async_trait;
use rusqlite;
use serde::{Deserialize, Serialize};
use serde_json;
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
}

impl SqliteDictionaryProvider {
  pub fn new(dictionary_path: Option<String>, freq_path: Option<String>) -> Self {
    let mut provider = Self {
      dictionary_path,
      freq_path,
      dictionary_conn: tokio::sync::Mutex::new(None),
      freq_conn: tokio::sync::Mutex::new(None),
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

  /// Asynchronously generates words with Levenshtein distance 1 and 2 from the input word
  async fn generate_levenshtein_candidates(
    &self,
    prefix: &str,
    include_distance_2: bool,
  ) -> Vec<String> {
    // Skip empty words or very long words
    if prefix.is_empty() {
      return ('a'..='z').map(|c| c.to_string()).collect();
    }

    // Limit max word length to avoid excessive processing
    if prefix.len() > 20 {
      return Vec::with_capacity(0);
    }

    // Calculate approximate capacity needed
    let char_count = prefix.chars().count();
    let distance_1_capacity = if char_count < 5 {
      // For short words, we'll generate fewer variations
      char_count + (char_count + 1) * 26 + char_count * 25 + char_count.saturating_sub(1)
    } else {
      // For longer words, use a more moderate capacity
      200
    };

    // For distance 2, we'll need much more space
    let total_capacity = if include_distance_2 {
      distance_1_capacity * 20
    } else {
      distance_1_capacity
    };

    // Use a HashSet to avoid duplicates when generating distance-2 words
    let mut result_set = std::collections::HashSet::with_capacity(total_capacity);
    result_set.insert(prefix.to_string());

    // STEP 1: Prioritize completions - Add characters only at the end
    // These are the highest priority candidates where prefix is preserved
    if prefix.is_ascii() {
      self.generate_prefix_completions_ascii(prefix, &mut result_set);
    } else {
      self.generate_prefix_completions_unicode(prefix, &mut result_set);
    }

    // Yield control to avoid blocking
    tokio::task::yield_now().await;

    // STEP 2: Only if we need more candidates, generate regular edit distance-1 words
    // but prioritize keeping the beginning of the word intact
    if prefix.is_ascii() {
      self.generate_distance_1_ascii_modified(prefix, &mut result_set);
    } else {
      self.generate_distance_1_unicode_modified(prefix, &mut result_set);
    }

    // Yield control periodically to avoid blocking
    tokio::task::yield_now().await;

    // Generate distance-2 words if requested
    if include_distance_2 {
      // Take distance-1 words and generate more edits
      let distance_1_vec: Vec<String> = result_set.iter().cloned().collect();

      // Process in chunks to allow yielding
      let chunk_size = 10;
      for chunk in distance_1_vec.chunks(chunk_size) {
        for base_word in chunk {
          // For distance-2, we only want to complete words that preserved our prefix
          if base_word.starts_with(prefix) {
            if base_word.is_ascii() {
              self.generate_prefix_completions_ascii(base_word, &mut result_set);
            } else {
              self.generate_prefix_completions_unicode(base_word, &mut result_set);
            }
          }
        }

        // Yield control after each chunk
        tokio::task::yield_now().await;
      }
    }

    // Convert to Vec for return
    result_set.into_iter().collect()
  }

  /// Generate only suffix completions for ASCII words (preserve prefix)
  fn generate_prefix_completions_ascii(
    &self,
    prefix: &str,
    result_set: &mut std::collections::HashSet<String>,
  ) {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    // Only add characters at the end (suffix completions)
    for c in b'a'..=b'z' {
      let mut new_word = Vec::with_capacity(word_len + 1);
      new_word.extend_from_slice(bytes);
      new_word.push(c);

      // Safety: we know the bytes are valid ASCII
      unsafe {
        result_set.insert(String::from_utf8_unchecked(new_word));
      }
    }
  }

  /// Generate only suffix completions for Unicode words (preserve prefix)
  fn generate_prefix_completions_unicode(
    &self,
    prefix: &str,
    result_set: &mut std::collections::HashSet<String>,
  ) {
    // Only add characters at the end (suffix completions)
    for c in 'a'..='z' {
      let mut new_word = String::with_capacity(prefix.len() + 1);
      new_word.push_str(prefix);
      new_word.push(c);
      result_set.insert(new_word);
    }
  }

  /// Modified ASCII word generator that prioritizes keeping the beginning intact
  fn generate_distance_1_ascii_modified(
    &self,
    prefix: &str,
    result_set: &mut std::collections::HashSet<String>,
  ) {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    // Insertion in middle (lower priority than suffix completions)
    for i in 0..word_len {
      for c in b'a'..=b'z' {
        let mut new_word = Vec::with_capacity(word_len + 1);
        new_word.extend_from_slice(&bytes[..i]);
        new_word.push(c);
        new_word.extend_from_slice(&bytes[i..]);

        // Safety: we know the bytes are valid ASCII
        unsafe {
          result_set.insert(String::from_utf8_unchecked(new_word));
        }
      }
    }

    // Only modify characters in the latter half of the word
    let modify_start = (word_len / 2).max(1);

    // Substitutions (only in latter half to preserve prefix)
    for i in modify_start..word_len {
      let original = bytes[i];
      for c in b'a'..=b'z' {
        if c != original {
          let mut new_word = bytes.to_vec();
          new_word[i] = c;

          // Safety: we know the bytes are valid ASCII
          unsafe {
            result_set.insert(String::from_utf8_unchecked(new_word));
          }
        }
      }
    }

    // Deletions (only in latter half to preserve prefix)
    for i in modify_start..word_len {
      let mut new_word = Vec::with_capacity(word_len - 1);
      new_word.extend_from_slice(&bytes[..i]);
      new_word.extend_from_slice(&bytes[i + 1..]);

      // Safety: we know the bytes are valid ASCII
      unsafe {
        result_set.insert(String::from_utf8_unchecked(new_word));
      }
    }
  }

  /// Modified Unicode word generator that prioritizes keeping the beginning intact
  fn generate_distance_1_unicode_modified(
    &self,
    prefix: &str,
    result_set: &mut std::collections::HashSet<String>,
  ) {
    let chars: Vec<char> = prefix.chars().collect();
    let char_len = chars.len();

    // Insertions in middle (lower priority than suffix completions)
    for i in 0..char_len {
      for c in 'a'..='z' {
        let mut new_word = String::with_capacity(prefix.len() + 1);
        for j in 0..i {
          new_word.push(chars[j]);
        }
        new_word.push(c);
        for j in i..char_len {
          new_word.push(chars[j]);
        }
        result_set.insert(new_word);
      }
    }

    // Only modify characters in the latter half of the word
    let modify_start = (char_len / 2).max(1);

    // Substitutions (only in latter half to preserve prefix)
    for i in modify_start..char_len {
      let original = chars[i];
      for c in 'a'..='z' {
        if c != original {
          let mut new_word = chars.clone();
          new_word[i] = c;
          result_set.insert(new_word.into_iter().collect());
        }
      }
    }

    // Deletions (only in latter half to preserve prefix)
    for i in modify_start..char_len {
      let mut new_word = String::with_capacity(prefix.len() - 1);
      for j in 0..char_len {
        if j != i {
          new_word.push(chars[j]);
        }
      }
      result_set.insert(new_word);
    }
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

  async fn find_words_by_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
    // Generate candidate words with Levenshtein distance 1
    // For better completions, we can include distance-2 if needed
    let include_distance_2 = true;
    let candidate_words = self
      .generate_levenshtein_candidates(prefix, include_distance_2)
      .await;

    if candidate_words.is_empty() {
      return Ok(None);
    }

    // Clone data needed for the blocking operation
    let freq_path = self.get_freq_path()?;

    // Process all candidates in one go since our generation is now more targeted
    let candidates_clone = candidate_words.clone();

    // Process database query in a blocking task
    let batch_results = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
      // Open the database connection within the blocking task
      let conn = rusqlite::Connection::open(&freq_path).map_err(|e| Error::internal_error())?;

      // Create placeholders for SQL query
      let placeholders = (0..candidates_clone.len())
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<String>>()
        .join(",");

      // Query with proper result limit
      // TODO：add some basical frequency threshold
      let query = format!(
        "SELECT word FROM word_frequencies WHERE word IN ({}) ORDER BY frequency DESC LIMIT 5",
        placeholders
      );

      let mut stmt = conn.prepare(&query).map_err(|_| Error::internal_error())?;

      // Convert words to SQL parameters
      let params: Vec<&dyn rusqlite::types::ToSql> = candidates_clone
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

    if batch_results.is_empty() {
      Ok(None)
    } else {
      Ok(Some(batch_results))
    }
  }
}

/// Provider implementation for JSON dictionaries
pub struct JsonDictionaryProvider {
  dictionary_path: Option<String>,
  freq_path: Option<String>,
  dictionary_cache: tokio::sync::Mutex<Option<serde_json::Value>>,
}

impl JsonDictionaryProvider {
  pub fn new(dictionary_path: Option<String>, freq_path: Option<String>) -> Self {
    let mut provider = Self {
      dictionary_path,
      freq_path,
      dictionary_cache: tokio::sync::Mutex::new(None),
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
      let matching_words: Vec<String> = entries
        .keys()
        .filter(|word| word.to_lowercase().starts_with(&prefix_lower))
        .take(50)
        .map(|word| word.clone())
        .collect();

      if !matching_words.is_empty() {
        return Ok(Some(matching_words));
      }
    }

    Ok(None)
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
