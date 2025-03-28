use dashmap::DashMap;
use fxhash::FxHasher;
use lru::LruCache;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::Hasher;
use std::num::NonZero;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::task;

fn create_cache_key(prefix: &str, include_distance_2: bool) -> u64 {
  let mut hasher = FxHasher::default();
  hasher.write(prefix.as_bytes());
  hasher.write_u8(include_distance_2 as u8);
  hasher.finish()
}

struct CacheEntry {
  value: Vec<String>,
  access_count: AtomicUsize,
}

impl CacheEntry {
  fn new(value: Vec<String>) -> Self {
    Self {
      value,
      access_count: AtomicUsize::new(1),
    }
  }

  fn increment_access(&self) {
    self.access_count.fetch_add(1, Ordering::Relaxed);
  }
}

static CANDIDATE_CACHE: Lazy<DashMap<u64, CacheEntry>> = Lazy::new(|| DashMap::with_capacity(1000));

thread_local! {
  static BUFFER_POOL: RefCell<Vec<Vec<u8>>> = RefCell::new(Vec::with_capacity(32));
}

static HOT_CACHE: Lazy<Mutex<LruCache<u64, Vec<String>>>> =
  Lazy::new(|| Mutex::new(LruCache::new(NonZero::new(100).unwrap())));

pub struct FuzzyMatcher;

impl FuzzyMatcher {
  pub async fn generate_candidates(prefix: String, include_distance_2: bool) -> Vec<String> {
    let cache_key = create_cache_key(&prefix, include_distance_2);

    {
      let mut hot_cache = HOT_CACHE.lock().unwrap();
      if let Some(result) = hot_cache.get(&cache_key) {
        return result.clone();
      }
    }

    if let Some(entry) = CANDIDATE_CACHE.get(&cache_key) {
      entry.increment_access();

      // if entry.access_count.load(Ordering::Relaxed) > 5 {
      //   let mut hot_cache = HOT_CACHE.lock().unwrap();
      //   if !hot_cache.contains(&cache_key) {
      //     hot_cache.put(cache_key, entry.value.clone());
      //   }
    }

    // return entry.value.clone();
    // }

    if prefix.is_empty() {
      return ('a'..='z').map(|c| c.to_string()).collect();
    }

    if prefix.len() > 20 {
      return Vec::new();
    }

    let capacity = if include_distance_2 {
      match prefix.len() {
        0..=2 => 600,
        3..=5 => 1200,
        _ => 2000,
      }
    } else {
      match prefix.len() {
        0..=2 => 160,
        3..=5 => 360,
        _ => 500,
      }
    };

    let mut result_set = HashSet::with_capacity(capacity);
    result_set.insert(prefix.clone());

    if prefix.is_ascii() {
      Self::generate_prefix_completions_ascii(&prefix, &mut result_set);
    } else {
      Self::generate_prefix_completions_unicode(&prefix, &mut result_set);
    }

    task::consume_budget().await;

    if prefix.is_ascii() {
      let prefix_clone = prefix.clone();
      let edit1_results =
        task::spawn_blocking(move || Self::generate_distance_1_ascii_parallel(&prefix_clone))
          .await
          .unwrap_or_default();

      result_set.extend(edit1_results);
    } else {
      Self::generate_distance_1_unicode_modified(&prefix, &mut result_set);
    }

    task::consume_budget().await;

    // if include_distance_2 && prefix.len() <= 8 {
    if include_distance_2 {
      let first_char_str = prefix.chars().next().unwrap_or('a').to_string();

      let base_words: Vec<String> = result_set
        .iter()
        .filter(|word| word.starts_with(&prefix) || word.starts_with(&first_char_str))
        .take((20.0 * (1.0 - (prefix.len() as f32) / 20.0)) as usize)
        .cloned()
        .collect();

      let futures = base_words
        .chunks(5)
        .map(|chunk| {
          let chunk_vec = chunk.to_vec();
          task::spawn_blocking(move || {
            chunk_vec
              .par_iter()
              // .with_min_len(if chunk_vec[0].len() > 4 { 2 } else { 1 })
              .flat_map(|base_word| {
                let mut local_set = HashSet::new();
                if base_word.is_ascii() {
                  Self::generate_prefix_completions_ascii(base_word, &mut local_set);
                } else {
                  Self::generate_prefix_completions_unicode(base_word, &mut local_set);
                }
                local_set.into_iter().collect::<Vec<_>>()
              })
              .collect::<Vec<_>>()
          })
        })
        .collect::<Vec<_>>();

      for future in futures {
        if let Ok(candidates) = future.await {
          result_set.extend(candidates);
        }
        task::consume_budget().await;
      }
    }

    let mut result: Vec<String> = result_set.into_iter().collect();

    let prefix_ref = &prefix;
    result.sort_by(|a, b| {
      let a_starts = a.starts_with(prefix_ref);
      let b_starts = b.starts_with(prefix_ref);

      match (a_starts, b_starts) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.len().cmp(&b.len()),
      }
    });

    let result: Vec<String> = result.into_iter().take(1000).collect();

    if !result.is_empty() && prefix.len() > 1 {
      if prefix.len() <= 5 || result.len() < 500 {
        CANDIDATE_CACHE.insert(cache_key, CacheEntry::new(result.clone()));
      }
    }

    result
  }

  fn generate_prefix_completions_ascii(prefix: &str, result_set: &mut HashSet<String>) {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    BUFFER_POOL.with(|pool| {
      let mut buffer = pool
        .borrow_mut()
        .pop()
        .unwrap_or_else(|| Vec::with_capacity(word_len + 1));
      buffer.clear();
      buffer.extend_from_slice(bytes);
      buffer.push(0);

      for c in b'a'..=b'z' {
        buffer[word_len] = c;
        if let Ok(new_word) = String::from_utf8(buffer.clone()) {
          result_set.insert(new_word);
        }
      }

      pool.borrow_mut().push(buffer);
    });
  }

  fn generate_prefix_completions_unicode(prefix: &str, result_set: &mut HashSet<String>) {
    let capacity = prefix.len() + 1;
    let candidates: Vec<String> = (b'a'..=b'z')
      .map(|c| {
        let mut new_word = String::with_capacity(capacity);
        new_word.push_str(prefix);
        new_word.push(c as char);
        new_word
      })
      .collect();

    result_set.extend(candidates);
  }

  fn generate_distance_1_ascii_parallel(prefix: &str) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    let chunk_size = match word_len {
      0..=3 => 1,
      4..=7 => 2,
      _ => 3,
    };

    let insertions = Self::generate_insertions_ascii_parallel(prefix, chunk_size);
    let substitutions = Self::generate_substitutions_ascii_parallel(prefix, chunk_size);
    let deletions = Self::generate_deletions_ascii_parallel(prefix, chunk_size);

    let mut result_set =
      HashSet::with_capacity(insertions.len() + substitutions.len() + deletions.len());

    result_set.extend(insertions);
    result_set.extend(substitutions);
    result_set.extend(deletions);

    result_set
  }

  fn generate_insertions_ascii_parallel(prefix: &str, chunk_size: usize) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();

    (0..=word_len)
      .collect::<Vec<_>>()
      .into_par_iter()
      .with_min_len(chunk_size)
      .flat_map(|i| {
        BUFFER_POOL.with(|pool| {
          let mut local_results = HashSet::with_capacity(26);
          let mut buffer = pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(word_len + 1));

          buffer.clear();
          buffer.extend_from_slice(&bytes[..i]);
          buffer.push(0);
          buffer.extend_from_slice(&bytes[i..]);

          for c in b'a'..=b'z' {
            buffer[i] = c;
            if let Ok(new_word) = String::from_utf8(buffer.clone()) {
              local_results.insert(new_word);
            }
          }

          pool.borrow_mut().push(buffer);

          local_results
        })
      })
      .collect()
  }

  fn generate_substitutions_ascii_parallel(prefix: &str, chunk_size: usize) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();
    let modify_start = (word_len / 2).max(1);

    (modify_start..word_len)
      .into_par_iter()
      .with_min_len(chunk_size)
      .flat_map(|i| {
        BUFFER_POOL.with(|pool| {
          let original = bytes[i];
          let mut local_results = HashSet::with_capacity(25);
          let mut buffer = pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(word_len));

          buffer.clear();
          buffer.extend_from_slice(bytes);

          for c in b'a'..=b'z' {
            if c != original {
              buffer[i] = c;
              if let Ok(new_word) = String::from_utf8(buffer.clone()) {
                local_results.insert(new_word);
              }
            }
          }

          pool.borrow_mut().push(buffer);
          local_results
        })
      })
      .collect()
  }

  fn generate_deletions_ascii_parallel(prefix: &str, chunk_size: usize) -> HashSet<String> {
    let bytes = prefix.as_bytes();
    let word_len = bytes.len();
    let modify_start = (word_len / 2).max(1);

    (modify_start..word_len)
      .into_par_iter()
      .with_min_len(chunk_size)
      .map(|i| {
        BUFFER_POOL.with(|pool| {
          let mut buffer = pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(word_len - 1));

          buffer.clear();
          buffer.extend_from_slice(&bytes[..i]);
          buffer.extend_from_slice(&bytes[i + 1..]);

          let result = String::from_utf8(buffer.clone()).unwrap_or_default();

          pool.borrow_mut().push(buffer);
          result
        })
      })
      .filter(|s| !s.is_empty())
      .collect()
  }

  fn generate_distance_1_unicode_modified(prefix: &str, result_set: &mut HashSet<String>) {
    let chars: Vec<char> = prefix.chars().collect();
    let char_len = chars.len();
    let modify_start = (char_len / 2).max(1);

    let alphabet: Vec<char> = (b'a'..=b'z').map(|c| c as char).collect();

    let expected_new_items =
      (char_len + 1) * 26 + (char_len - modify_start) * 25 + (char_len - modify_start);

    if result_set.capacity() < result_set.len() + expected_new_items {
      result_set.reserve(expected_new_items);
    }

    for i in 0..=char_len {
      for &c in &alphabet {
        let mut new_word = String::with_capacity(char_len + 1);
        new_word.extend(chars[..i].iter());
        new_word.push(c);
        new_word.extend(chars[i..].iter());
        result_set.insert(new_word);
      }
    }

    for i in modify_start..char_len {
      let original = chars[i];
      for &c in &alphabet {
        if c != original {
          let mut new_word = String::with_capacity(char_len);
          new_word.extend(chars[..i].iter());
          new_word.push(c);
          new_word.extend(chars[i + 1..].iter());
          result_set.insert(new_word);
        }
      }
    }

    for i in modify_start..char_len {
      let mut new_word = String::with_capacity(char_len - 1);
      new_word.extend(chars[..i].iter());
      new_word.extend(chars[i + 1..].iter());
      result_set.insert(new_word);
    }
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
  FuzzyMatcher::generate_candidates(prefix.to_string(), include_distance_2).await
}
