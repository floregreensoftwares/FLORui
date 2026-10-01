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

## Where a frame's time goes

`florui-bench phases NAME [--ops N] [--out FILE.md]` runs one workload in this process with the
profiler started, drops the first run and a few warm-up runs, and prints the median time and calls
of each phase over the rest, plus the counters of the last frame (nodes styled, laid out and
painted, memo hits). It needs a build with the profiler:

```
cargo build --release -p florui-bench --features profiling
target/release/florui-bench phases frame_1k_rows
```

`ab` says whether a change moved a workload; this says in which phase. A phase can contain others
(an update contains render, cascade and layout), so the rows do not add up to the frame. The
profiler summarizes a frame when one is painted, so a workload that only updates (`update_*`) has
none and the command says so; use the matching frame workload.

## Profiler overhead

`florui-bench overhead` measures what one profiler span and counter update cost in each mode (idle,
summary, detail); it needs a build with `--features profiling`. To compare whole workloads with the
profiler compiled out against built in, build the binary twice (with and without the feature) and
run `ab`; the variable `FLORUI_BENCH_PROFILER=summary` (or `detail`) starts the profiler in the
second build. `overhead/profiler-overhead.md` has the recorded result.

## Collections and what a component owns

`virtual_list_*` mounts the same row view in a virtualized list over 1,000, 10,000 and 100,000
items, so a difference between the three is the cost of the data size alone; what a frame mounts is
the same in all of them (`florui-bench phases` reports it: 81 nodes styled and laid out for any
data size). `virtual_list_10k_variable` has rows whose height depends on their text, which are
measured after they mount and correct the list's estimate, and `virtual_list_jump_10k_variable`
jumps to an item thousands of rows away where nothing was measured. A sample of those is one 100 px
wheel tick (forward for 100 ticks, then back) or one jump, then a paint.

`mount_unmount_300_components` unmounts and mounts again 300 components that each keep a signal, a
memo and an effect with a cleanup; one sample is the whole cycle, since alternating the two halves
would make the median a mix of a cheap and an expensive operation.

Timing cannot show that unmounting released what mounting created, so the crate's tests check it
with the live-count feature of `florui-reactive` (a dev-dependency only: tracking every scope and
signal in the benchmark binary would slow the workloads). They check that a list holds the same
scopes whatever the data size, that scrolling away and back 400 ticks accumulates no row state, and
that five unmount and mount cycles leave nothing behind.

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
- `collections-f7c0223`: only the virtualized list and mount-cycle workloads, 5 processes, on the
  commit that adds them (a rebase can rewrite its hash; the engine is `grow/main` as of the cascade
  style sharing and the layout measure memo). A wheel tick costs 11.7 ms over 1,000, 10,000 and
  100,000 items with the same 5,517 allocations and 6,963 KiB per operation, and the peak heap grows
  with the data (6.7, 6.9 and 9.8 MiB): the data size costs memory, not time. Variable rows 22.3 ms,
  a jump 22.5 ms, a mount and unmount cycle of 300 components 21.5 ms. Between-process spread is 3%
  to 10% except the 100,000-item list, which showed 19.9%.

Repeat a measurement on a quiet machine before trusting a small difference.
