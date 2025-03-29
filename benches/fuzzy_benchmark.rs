use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use dictionary_lsp::fuzzy::FuzzyMatcher;
use std::time::Duration;
use tokio::runtime::Runtime;

fn bench_fuzzy_matcher(c: &mut Criterion) {
  let rt = Runtime::new().unwrap();

  // Create benchmark group with meaningful configuration
  let mut group = c.benchmark_group("fuzzy_matching");
  group.measurement_time(Duration::from_secs(10));
  group.sample_size(50);

  // ASCII benchmarks with different lengths
  let ascii_prefixes = ["a", "ab", "abc", "abcd", "abcde"];

  for prefix in ascii_prefixes.iter() {
    // Benchmark distance-1 only
    group.bench_with_input(
      BenchmarkId::new("ascii_dist1", prefix),
      prefix,
      |b, prefix| {
        b.to_async(&rt)
          .iter(|| async { FuzzyMatcher::generate_candidates(prefix.to_string(), false).await });
      },
    );

    // Benchmark with distance-2
    group.bench_with_input(
      BenchmarkId::new("ascii_dist2", prefix),
      prefix,
      |b, prefix| {
        b.to_async(&rt)
          .iter(|| async { FuzzyMatcher::generate_candidates(prefix.to_string(), true).await });
      },
    );
  }

  // Unicode benchmarks
  let unicode_prefixes = ["á", "áb", "ábc", "ábcd"];

  for prefix in unicode_prefixes.iter() {
    group.bench_with_input(
      BenchmarkId::new("unicode_dist1", prefix),
      prefix,
      |b, prefix| {
        b.to_async(&rt)
          .iter(|| async { FuzzyMatcher::generate_candidates(prefix.to_string(), false).await });
      },
    );

    group.bench_with_input(
      BenchmarkId::new("unicode_dist2", prefix),
      prefix,
      |b, prefix| {
        b.to_async(&rt)
          .iter(|| async { FuzzyMatcher::generate_candidates(prefix.to_string(), true).await });
      },
    );
  }

  group.finish();
}

criterion_group!(benches, bench_fuzzy_matcher);
criterion_main!(benches);
