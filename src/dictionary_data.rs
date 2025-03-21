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
pub fn create_dictionary_provider(dictionary_path: Option<String>) -> Box<dyn DictionaryProvider> {
  if Config::is_sqlite(dictionary_path.as_deref()) {
    Box::new(SqliteDictionaryProvider::new(dictionary_path))
  } else {
    Box::new(JsonDictionaryProvider::new(dictionary_path))
  }
}

/// Provider implementation for SQLite dictionaries
pub struct SqliteDictionaryProvider {
  dictionary_path: Option<String>,
}

impl SqliteDictionaryProvider {
  pub fn new(dictionary_path: Option<String>) -> Self {
    Self { dictionary_path }
  }
  fn get_dictionary_path(&self) -> Result<String> {
    match &self.dictionary_path {
      Some(path) => Ok(path.clone()),
      None => Err(Error::invalid_params("Dictionary path not provided")),
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
}

#[async_trait]
impl DictionaryProvider for SqliteDictionaryProvider {
  async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>> {
    let word_lower = word;

    let dict_path = &self.get_dictionary_path()?;

    let conn = match rusqlite::Connection::open(&dict_path) {
      Ok(conn) => conn,
      Err(e) => {
        eprintln!("Error connecting to SQLite database: {}", e);
        return Err(Error::internal_error());
      }
    };

    if let Some(response) = self.find_exact_match(&conn, &word_lower)? {
      return Ok(Some(response));
    }

    if let Some(response) = self.find_fuzzy_match(&conn, &word_lower)? {
      return Ok(Some(response));
    }

    // No matches found
    Ok(None)
  }

  fn get_word_at_position(&self, content: &str, position: Position) -> Option<String> {
    extract_word_at_position(content, position)
  }

  async fn find_words_by_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
    let dict_path = &self.get_dictionary_path()?;

    let conn = match rusqlite::Connection::open(&dict_path) {
      Ok(conn) => conn,
      Err(e) => {
        eprintln!("Error connecting to SQLite database: {}", e);
        return Err(Error::internal_error());
      }
    };

    let query = "SELECT word FROM words WHERE word LIKE ?1 || '%' AND length(word) < 20 AND word GLOB '[A-Za-z]*' AND word NOT GLOB '*[^A-Za-z]*' LIMIT 2";

    let mut stmt = match conn.prepare(query) {
      Ok(stmt) => stmt,
      Err(e) => {
        eprintln!("Error preparing statement: {}", e);
        return Err(Error::internal_error());
      }
    };

    let param = format!("{}%", prefix);
    let rows = stmt.query_map([param], |row| row.get::<_, String>(0));

    match rows {
      Ok(mapped_rows) => {
        let mut words = Vec::new();
        for word_result in mapped_rows {
          match word_result {
            Ok(word) => words.push(word),
            Err(e) => {
              eprintln!("Error retrieving word: {}", e);
              return Err(Error::internal_error());
            }
          }
        }

        if words.is_empty() {
          Ok(None)
        } else {
          Ok(Some(words))
        }
      }
      Err(e) => {
        eprintln!("Error executing query: {}", e);
        Err(Error::internal_error())
      }
    }
  }
}

/// Provider implementation for JSON dictionaries
pub struct JsonDictionaryProvider {
  dictionary_path: Option<String>,
}

impl JsonDictionaryProvider {
  pub fn new(dictionary_path: Option<String>) -> Self {
    Self { dictionary_path }
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
    let dict_path = self.get_dictionary_path()?;
    let dictionary = self.read_dictionary_file(&dict_path)?;

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
    let dict_path = self.get_dictionary_path()?;
    let dictionary = self.read_dictionary_file(&dict_path)?;

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
