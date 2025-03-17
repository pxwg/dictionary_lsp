use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tower_lsp::jsonrpc::{Error, Result};

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

#[derive(Debug, Serialize, Deserialize)]
pub struct DictionaryResponse {
    pub word: String,
    pub meanings: Vec<Meaning>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Meaning {
    pub part_of_speech: String,
    pub definitions: Vec<Definition>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Definition {
    pub definition: String,
    pub example: Option<String>,
}

pub struct DictionaryLoader {
    dictionary_path: Option<String>,
}

impl DictionaryLoader {
    pub fn new(dictionary_path: Option<String>) -> Self {
        Self { dictionary_path }
    }

    /// Retrieves the dictionary definition for a given word.
    /// Reads from a local JSON dictionary file and parses the entries.
    pub async fn get_meaning(&self, word: &str) -> Result<Option<DictionaryResponse>> {
        let dict_path = self.get_dictionary_path()?;
        let dictionary = self.read_dictionary_file(&dict_path)?;

        let word_lower = word.to_lowercase();

        // Try exact match first
        if let Some(response) = self.find_exact_match(&dictionary, &word_lower) {
            return Ok(Some(response));
        }

        // Fall back to fuzzy matching
        if let Some(response) = self.find_fuzzy_match(&dictionary, &word_lower) {
            return Ok(Some(response));
        }

        Ok(None)
    }

    /// Determines the path to the dictionary file based on configuration or defaults.
    fn get_dictionary_path(&self) -> Result<PathBuf> {
        let dict_path = if let Some(path) = &self.dictionary_path {
            PathBuf::from(path)
        } else {
            match dirs::home_dir().map(|p| p.join("dicts/dictionary.json")) {
                Some(path) => path,
                None => {
                    eprintln!("Could not determine home directory");
                    return Err(Error::internal_error());
                }
            }
        };

        if !dict_path.exists() {
            eprintln!("Dictionary file not found at {:?}", dict_path);
            return Err(Error::internal_error());
        }

        Ok(dict_path)
    }

    /// Reads and parses the dictionary file into a JSON value.
    fn read_dictionary_file(&self, dict_path: &Path) -> Result<serde_json::Value> {
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

    /// Finds an exact match for the word in the dictionary.
    fn find_exact_match(
        &self,
        dictionary: &serde_json::Value,
        word: &str,
    ) -> Option<DictionaryResponse> {
        dictionary
            .get(word)
            .map(|entry| self.parse_dictionary_entry(word, entry, Some(word)))
    }

    /// Attempts to find a close match using fuzzy matching.
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

        closest_match.map(|(matched_word, entry)| {
            self.parse_dictionary_entry(&matched_word, entry, Some(word))
        })
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

    // Parse dictionary entry into DictionaryResponse format
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

    /// Extracts the word at the given cursor position in the document.
    /// Handles both alphabetic characters and CJK characters properly.
    pub fn get_word_at_position(
        &self,
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
}
