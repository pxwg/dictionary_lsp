use dashmap::DashMap;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use tokio::task;

static CANDIDATE_CACHE: Lazy<DashMap<String, Vec<String>>> =
  Lazy::new(|| DashMap::with_capacity(1000));

pub struct FuzzyMatcher;

impl FuzzyMatcher {
  pub async fn generate_candidates(prefix: &str, include_distance_2: bool) -> Vec<String> {
    let cache_key = format!("{}:{}", prefix, include_distance_2);
    if let Some(cached) = CANDIDATE_CACHE.get(&cache_key) {
      return cached.clone();
    }

    if prefix.is_empty() {
      return ('a'..='z').map(|c| c.to_string()).collect();
    }

    if prefix.len() > 20 {
      return Vec::with_capacity(0);
    }

    let char_count = prefix.chars().count();
    let capacity = match (char_count, include_distance_2) {
      (0..=2, true) => 300,
      (3..=5, true) => 700,
      (_, true) => 1000,
      (0..=2, false) => 80,
      (3..=5, false) => 180,
      _ => 250,
    };

    let mut result_set = HashSet::with_capacity(capacity);
    result_set.insert(prefix.to_string());

    if prefix.is_ascii() {
      Self::generate_prefix_completions_ascii(prefix, &mut result_set);
    } else {
      Self::generate_prefix_completions_unicode(prefix, &mut result_set);
    }

    task::yield_now().await;

    if prefix.is_ascii() {
      let prefix_owned = prefix.to_owned();
      let edit1_results =
        task::spawn_blocking(move || Self::generate_distance_1_ascii_parallel(&prefix_owned))
          .await
          .unwrap_or_default();

      result_set.extend(edit1_results);
    } else {
      Self::generate_distance_1_unicode_modified(prefix, &mut result_set);
    }

    task::yield_now().await;

    if include_distance_2 && prefix.len() <= 8 {
      let base_words: Vec<String> = result_set
        .iter()
        .filter(|word| word.starts_with(prefix))
        .take(20)
        .cloned()
        .collect();

      for chunk in base_words.chunks(5) {
        let chunk_vec = chunk.to_vec();
        let new_candidates = task::spawn_blocking(move || {
          let local_results: Vec<String> = chunk_vec
            .par_iter()
            .flat_map(|base_word| {
              let mut local_set = HashSet::new();
              if base_word.is_ascii() {
                Self::generate_prefix_completions_ascii(base_word, &mut local_set);
              } else {
                Self::generate_prefix_completions_unicode(base_word, &mut local_set);
              }
              local_set.into_iter().collect::<Vec<_>>()
            })
            .collect();
          local_results
        })
        .await
        .unwrap_or_default();

        result_set.extend(new_candidates);
        task::yield_now().await;
      }
    }

    let result: Vec<String> = result_set.into_iter().take(1000).collect();

    if result.len() > 0 && result.len() < 500 && prefix.len() > 1 {
      CANDIDATE_CACHE.insert(cache_key, result.clone());
    }

    result
  }

  fn generate_prefix_completions_ascii(prefix: &str, result_set: &mut HashSet<String>) {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    // Create all possible completions in one batch
    let candidates: Vec<String> = (b'a'..=b'z')
      .filter_map(|c| {
        let mut buffer = Vec::with_capacity(word_len + 1);
        buffer.extend_from_slice(bytes);
        buffer.push(c);
        String::from_utf8(buffer).ok()
      })
      .collect();

    result_set.extend(candidates);
  }

  fn generate_prefix_completions_unicode(prefix: &str, result_set: &mut HashSet<String>) {
    // Generate all completions at once
    let candidates: Vec<String> = (b'a'..=b'z')
      .map(|c| {
        let mut new_word = String::with_capacity(prefix.len() + 1);
        new_word.push_str(prefix);
        new_word.push(c as char);
        new_word
      })
      .collect();

    result_set.extend(candidates);
  }

  fn generate_distance_1_ascii_parallel(prefix: &str) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let _word_len = bytes.len();

    let insertions = Self::generate_insertions_ascii_parallel(prefix);
    let substitutions = Self::generate_substitutions_ascii_parallel(prefix);
    let deletions = Self::generate_deletions_ascii_parallel(prefix);

    let mut result_set =
      HashSet::with_capacity(insertions.len() + substitutions.len() + deletions.len());

    result_set.extend(insertions);
    result_set.extend(substitutions);
    result_set.extend(deletions);

    result_set
  }

  fn generate_insertions_ascii_parallel(prefix: &str) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    (0..=word_len)
      .into_par_iter()
      .flat_map(|i| {
        let mut local_results = HashSet::new();
        for c in b'a'..=b'z' {
          let mut buffer = Vec::with_capacity(word_len + 1);
          buffer.extend_from_slice(&bytes[..i]);
          buffer.push(c);
          buffer.extend_from_slice(&bytes[i..]);

          if let Ok(new_word) = String::from_utf8(buffer) {
            local_results.insert(new_word);
          }
        }
        local_results
      })
      .collect()
  }

  fn generate_substitutions_ascii_parallel(prefix: &str) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();
    let modify_start = (word_len / 2).max(1);

    (modify_start..word_len)
      .into_par_iter()
      .flat_map(|i| {
        let original = bytes[i];
        let mut local_results = HashSet::new();

        for c in b'a'..=b'z' {
          if c != original {
            let mut buffer = bytes.to_vec();
            buffer[i] = c;

            if let Ok(new_word) = String::from_utf8(buffer) {
              local_results.insert(new_word);
            }
          }
        }
        local_results
      })
      .collect()
  }

  fn generate_deletions_ascii_parallel(prefix: &str) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();
    let modify_start = (word_len / 2).max(1);

    (modify_start..word_len)
      .into_par_iter()
      .map(|i| {
        let mut buffer = Vec::with_capacity(word_len - 1);
        buffer.extend_from_slice(&bytes[..i]);
        buffer.extend_from_slice(&bytes[i + 1..]);

        String::from_utf8(buffer).unwrap_or_default()
      })
      .filter(|s| !s.is_empty())
      .collect()
  }

  fn generate_distance_1_unicode_modified(prefix: &str, result_set: &mut HashSet<String>) {
    let chars: Vec<char> = prefix.chars().collect();
    let char_len = chars.len();
    let alphabet: Vec<char> = (b'a'..=b'z').map(|c| c as char).collect();

    // Process insertions
    let insertions: Vec<String> = (0..=char_len)
      .flat_map(|i| {
        let chars_clone = chars.clone(); // Clone to avoid ownership issues
        alphabet.iter().map(move |&c| {
          let mut new_word = String::with_capacity(char_len + 1);
          new_word.extend(chars_clone[..i].iter());
          new_word.push(c);
          new_word.extend(chars_clone[i..].iter());
          new_word
        })
      })
      .collect();
    result_set.extend(insertions);

    let modify_start = (char_len / 2).max(1);

    // Process substitutions
    let substitutions: Vec<String> = (modify_start..char_len)
      .flat_map(|i| {
        let chars_clone = chars.clone(); // Clone for this scope
        let original = chars_clone[i];
        alphabet
          .iter()
          .filter(move |&&c| c != original) // Use move to capture original
          .map(move |&c| {
            let mut new_word = chars_clone.clone();
            new_word[i] = c;
            new_word.iter().collect()
          })
      })
      .collect();
    result_set.extend(substitutions);

    // Process deletions
    let deletions: Vec<String> = (modify_start..char_len)
      .map(|i| {
        let chars_clone = chars.clone(); // Clone for this scope too
        chars_clone
          .iter()
          .enumerate()
          .filter_map(|(j, &c)| if j != i { Some(c) } else { None })
          .collect()
      })
      .collect();
    result_set.extend(deletions);
  }
}

/// Function for generate levenshtein candidates
/// ## Parameters
/// - `prefix`: &str - Prefix to generate candidates
/// - `include_distance_2`: bool - Include distance 2 candidates
/// ## Returns
/// - Vec<String> - Vector of candidates
pub async fn generate_levenshtein_candidates(
  prefix: &str,
  include_distance_2: bool,
) -> Vec<String> {
  FuzzyMatcher::generate_candidates(prefix, include_distance_2).await
}
