# florui-bench

Reproducible performance baselines for the frame pipeline, and a comparison
that tells a real change from noise. It complements the per-crate Criterion
benches: those time one function in one process; this measures whole
workloads (a list updating, a scroll, a first frame) the way a window runs
them, each in its own process so no run inherits another's caches.

## What a run records

- Every workload runs in separate child processes, in alternating order from
  round to round. The first operation of a fresh process is the **cold**
  sample; after three discarded runs the **warm** samples follow (up to 100,
  or 8 seconds).
- Every raw sample is kept in the JSON report, with the commit, compiler, OS,
  CPU, power plan and build profile.
- Heap figures (allocations and bytes per operation, peak live bytes) come
  from one extra run per workload with a counting allocator, so counting never
  affects a timing.
- Tail percentiles appear only with enough samples (p95 from 20, p99 from
  100).
- "Between processes" is the spread of each process's own median. A large
  value means the run does not reproduce itself on that machine at that time;
  do not draw conclusions from differences smaller than it.

## Running it

```
cargo build --release -p florui-bench
target\release\florui-bench run --label my-run --out out\my-run.json
```

Compare two builds on the same machine, alternating them round by round so
drift and ordering affect both alike:

```
target\release\florui-bench ab --a base.exe --b candidate.exe --rounds 5 --out-dir out\ab
```

A change counts when it exceeds 5% and its 95% bootstrap interval excludes
zero (`--threshold` adjusts it). Use a release build, close other builds and
heavy programs, and keep the same power plan.

## Profiler overhead

`florui-bench overhead` measures what one profiler span and counter update cost in each mode (idle,
summary, detail); it needs a build with `--features profiling`. To compare whole workloads with the
profiler compiled out against built in, build the binary twice (with and without the feature) and
run `ab`; the variable `FLORUI_BENCH_PROFILER=summary` (or `detail`) starts the profiler in the
second build. `overhead/profiler-overhead.md` has the recorded result.

## What it does not measure

The workloads run in a headless window: update, layout, paint and the
accessibility tree, all CPU work. Presentation and GPU time are not included
and are measured separately. Results are for the machine in the report; compare
only runs from the same machine.

## Recorded baselines

`baselines/` holds the reports of runs on the development machine, with raw samples. Compare a
change against the newest one that matches the code it is based on; a baseline goes stale as the
engine improves.

- `frame-pipeline-ac1e72b`: the first, before any performance work. Taken while other builds ran
  on the same machine at times, which shows as up to 19% spread between processes.
- `frame-pipeline-ec5815a`: the engine after the paint culling, clip-mask reuse, style and
  accessibility work, on a quieter machine (between-process spread 1% to 5% on the 1,000-row and
  larger workloads, 18% to 23% on the 100-row and deep-tree ones, so a small change there needs more
  processes before it can be trusted). Updating 1,000 rows went from 95.6 ms to 52.5 ms and a
  wheel notch over a scroll box from 476 ms to 27 ms; inline text is unchanged.
- `scroll-hover-ebd9729`: only `scroll_1k_rows` and `hover_1k_rows`, 8 processes, after those two
  workloads were fixed. The `scroll_1k_rows` and `hover_1k_rows` rows of the two reports above
  measure a window where nothing moved (the scroll box was not registered, so it never scrolled, and
  the hover pointer sat over a child, so no row was restyled); use this one for those two. A real
  100 px wheel tick over 1,000 rows costs about 96 ms, not the 27 ms the earlier figure suggested.

Repeat a measurement on a quiet machine before trusting a small difference.
