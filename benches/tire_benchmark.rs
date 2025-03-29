use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::path::Path;
use std::time::Instant;

// Import necessary items from the project
extern crate dictionary_lsp;
use dictionary_lsp::tire;

fn initialize_trie() -> Result<(), String> {
  // Use the test database file in the project directory
  let freq_db_path = Path::new("test/test_freq_large.db");

  if !freq_db_path.exists() {
    return Err(format!(
      "Database file not found at {}",
      freq_db_path.display()
    ));
  }

  tire::initialize_global_trie(freq_db_path.to_str().unwrap())
    .map_err(|e| format!("Failed to initialize trie: {:?}", e))?;

  if !tire::is_trie_initialized() {
    return Err("Trie initialization reported success but trie is not initialized".to_string());
  }

  Ok(())
}

fn benchmark_find_words_by_prefix(c: &mut Criterion) {
  // Initialize the trie before benchmarking
  match initialize_trie() {
    Ok(_) => println!("Trie initialized successfully"),
    Err(e) => {
      println!("Error initializing trie: {}", e);
      return;
    }
  }

  // Define test cases with various prefixes
  let test_cases = vec![
    ("a", "Short common prefix"),
    ("pro", "Medium common prefix"),
    ("compre", "Longer specific prefix"),
    ("z", "Uncommon letter prefix"),
    ("xyl", "Rare prefix"),
  ];

  let mut group = c.benchmark_group("find_words_by_prefix");
  for (prefix, description) in test_cases {
    group.bench_function(format!("{}: '{}'", description, prefix), |b| {
      b.iter(|| black_box(tire::find_words_by_prefix(black_box(prefix), black_box(10))));
    });
  }
  group.finish();
}

fn benchmark_find_words_respecting_case(c: &mut Criterion) {
  // Define test cases with various prefixed and capitalization patterns
  let test_cases = vec![
    ("pro", "Lowercase prefix"),
    ("Pro", "Capitalized prefix"),
    ("com", "Lowercase common prefix"),
    ("Com", "Capitalized common prefix"),
    ("Z", "Capitalized uncommon letter"),
  ];

  let mut group = c.benchmark_group("find_words_respecting_case");
  for (prefix, description) in test_cases {
    group.bench_function(format!("{}: '{}'", description, prefix), |b| {
      b.iter(|| {
        black_box(tire::find_words_respecting_case(
          black_box(prefix),
          black_box(10),
        ))
      });
    });
  }
  group.finish();
}

fn benchmark_trie_fill_rate() {
  // Check how many words are in each trie
  let high_count = match &*tire::HIGH_FREQ_TRIE.read().unwrap() {
    Some(trie) => {
      let start = Instant::now();
      let count = trie
        .predictive_search(&vec![])
        .collect::<Vec<Vec<char>>>()
        .len();
      println!(
        "HIGH_FREQ_TRIE has {} words (listed in {:?})",
        count,
        start.elapsed()
      );
      count
    }
    None => 0,
  };

  let mid_count = match &*tire::MID_FREQ_TRIE.read().unwrap() {
    Some(trie) => {
      let start = Instant::now();
      let count = trie
        .predictive_search(&vec![])
        .collect::<Vec<Vec<char>>>()
        .len();
      println!(
        "MID_FREQ_TRIE has {} words (listed in {:?})",
        count,
        start.elapsed()
      );
      count
    }
    None => 0,
  };

  let low_count = match &*tire::LOW_FREQ_TRIE.read().unwrap() {
    Some(trie) => {
      let start = Instant::now();
      let count = trie
        .predictive_search(&vec![])
        .collect::<Vec<Vec<char>>>()
        .len();
      println!(
        "LOW_FREQ_TRIE has {} words (listed in {:?})",
        count,
        start.elapsed()
      );
      count
    }
    None => 0,
  };

  println!(
    "Total words in tries: {}",
    high_count + mid_count + low_count
  );
}

fn benchmark_cache_effectiveness(c: &mut Criterion) {
  let mut group = c.benchmark_group("cache_effectiveness");

  // First lookup (cold cache)
  group.bench_function("First lookup (cold cache)", |b| {
    b.iter(|| {
      // Clear cache between iterations
      {
        let mut cache = tire::PREFIX_CACHE.write().unwrap();
        cache.clear();
      }
      black_box(tire::find_words_by_prefix(black_box("pro"), black_box(10)))
    });
  });

  // Second lookup (warm cache)
  group.bench_function("Second lookup (warm cache)", |b| {
    b.iter_batched(
      || {
        // Setup: do first lookup to fill cache
        tire::find_words_by_prefix("pro", 10);
      },
      |_| {
        // Benchmark the cached lookup
        black_box(tire::find_words_by_prefix(black_box("pro"), black_box(10)))
      },
      criterion::BatchSize::SmallInput,
    );
  });

  group.finish();
}

fn criterion_benchmark(c: &mut Criterion) {
  // Print some diagnostic information about the tries
  benchmark_trie_fill_rate();

  // Run the actual benchmarks
  benchmark_find_words_by_prefix(c);
  benchmark_find_words_respecting_case(c);
  benchmark_cache_effectiveness(c);
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
