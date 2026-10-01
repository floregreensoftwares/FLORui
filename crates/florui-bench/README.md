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

`baselines/` holds the reports of runs on the development machine, with raw
samples. The first was taken while other builds ran on the same machine at
times, which shows as a between-process spread of up to 19% on some workloads;
repeat a measurement on a quiet machine before trusting a small difference.
