# Indexing performance

This report records the bounded indexing spike tracked by
https://github.com/benediktms/beholder/issues/6. Measurements were taken on
2026-08-16 using an Apple M3 Max with 36 GiB RAM on arm64 macOS.

## Reproducing the benchmark

The benchmark creates an isolated database and frontend cache, performs a cold
index, then changes only the resolver identity to exercise warm frontend caches
and repository-state reuse.

```sh
just index-bench 4 "/path/to/repository-a:/path/to/repository-b"
```

The path list uses the platform path separator. The benchmark reports pipeline
stage timings and uses the platform `time` command for CPU and peak RSS.

## Mnestic publication batching

Beholder previously issued up to three Mnestic scripts per observation. A
single transaction now submits observations in batches of 10,000 rows while
preserving the same relations and atomic workspace revision.

| Corpus | Publication | Before | Batched | Change |
| --- | ---: | ---: | ---: | ---: |
| 21,000 dependency facts | cold | 3,464 ms | 436 ms | 7.9x faster |
| 21,000 dependency facts | reusable state | 196 ms | 206 ms | no material change |
| 181,734 observations | cold | 34,969 ms | 6,882 ms | 5.1x faster |

A 100,000-row batch was slower on the large corpus (7,538 ms publication) and
raised peak RSS to 1,757 MiB. The 10,000-row batch used 1,385 MiB, close to the
1,323 MiB unbatched baseline.

## Worker-count results

The production-scale corpus contained three repositories, 11,115 source units,
181,734 observations, and 13,776 identical known frontend diagnostics at every
worker count.

| Workers | Cold total | Load | Analysis | Publish | Checkpoint | Warm total | CPU | Peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 16,334 ms | 2,379 ms | 6,386 ms | 6,882 ms | 629 ms | 7,759 ms | 90% | 1,385 MiB |
| 2 | 12,136 ms | 1,770 ms | 3,384 ms | 6,348 ms | 581 ms | 6,850 ms | 115% | 1,376 MiB |
| 4 | 10,655 ms | 1,703 ms | 1,932 ms | 6,379 ms | 591 ms | 6,805 ms | 126% | 1,387 MiB |
| 8 | 10,138 ms | 1,765 ms | 1,222 ms | 6,530 ms | 569 ms | 6,711 ms | 135% | 1,395 MiB |

CPU is `(user + system) / wall` for the complete cold and warm process.
Observation and diagnostic counts matched at every worker count. A targeted test
asserts identical serialized repository analysis and context query output at one
and eight workers.

The small corpus contained one repository, 45 source units, and 5,464
observations.

| Workers | Cold total | Analysis | Warm total | Peak RSS |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 194 ms | 53 ms | 67 ms | 107 MiB |
| 2 | 177 ms | 32 ms | 70 ms | 105 MiB |
| 4 | 160 ms | 20 ms | 68 ms | 106 MiB |
| 8 | 157 ms | 13 ms | 69 ms | 106 MiB |

Parallel analysis saves only 37 ms on the small cold corpus and does not improve
the warm path. On the large cold corpus, two workers save 4.2 seconds, four save
another 1.5 seconds, and eight save only another 0.5 seconds.

## Recommendation

- Keep Mnestic publication and workspace resolution single-threaded.
- Use a bounded four-worker pool by default, capped by available parallelism.
- Allow `BEHOLDER_INDEX_WORKERS` to override the worker count for constrained
  machines and benchmarking.
- Keep 10,000-row Mnestic publication batches.
- Treat RocksDB or broader storage changes as separate work only if publication
  remains a measured bottleneck after incremental indexing is implemented.
