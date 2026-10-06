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

## Against the base, on every pull request

`scripts/bench-pr.ps1 -Base <sha> -Head <sha>` builds both commits, alternates them with `ab` and
writes `report.md`. The workflow `bench.yml` runs it for each pull request and posts the result as
one comment, updated in place. It is not a required check and never fails a pull request: a shared
runner is too noisy for a number to mean anything alone, so each run compares a commit with its base
on the same machine, not with a stored figure.

`ab --alarm 0.10` also writes `alarms.json` (and `alarms.md` when there is something): the
workloads slower by 10% or more, past the usual 5% bar with an interval that excludes zero. A pull
request with one gets the `perf-alert` label, so `label:perf-alert` lists what is worth looking at.
It is a flag to rerun and open the profiler, not a verdict.

The job runs only the workloads of a thousand rows and more, five rounds. Two identical builds run
this way raised no alarm and moved at most 4.5%; the same two builds on 100-row workloads with two
rounds raised a false one at +17%, which is why the small workloads and short runs are left out.

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

## Composition effects

`effects_*` paint busy content (a window of colored tiles) with absolutely placed panels over it.
`effects_opaque_large` is the cheapest way to cover 600 x 400 pixels and `effects_translucent_*` is the
same panel translucent with no filter, which is the equivalent work without an effect: the
difference between a filtered panel and the translucent one of its own area and content is what the
effect costs. The filtered ones vary the area (200 x 150 and 600 x 400), the `backdrop-filter` blur
radius (8 and 24 px), the overlap (three panels), the display scale (1 and 2), and use `filter: blur`
on the element and a chain of color filters (`brightness`, `contrast`, `saturate`).

Each effect workload asserts when it is built that it paints differently from its translucent
baseline, so a declaration the engine ignores cannot be measured as the baseline. A sample is a full
paint of the scene; the engine repaints everything, so a moving background costs the same as a still
one. The heap columns are the intermediate memory an effect needs. These are CPU times of the
headless path: GPU time and presentation are not included.

## Edit to present

`florui-bench edit-latency [--rows N] [--rounds N] [--write in-place|atomic] [--out-dir DIR]` opens a
real window in a child process, rewrites its stylesheet and reports how long it takes until the new
color is composed on the screen, split into intervals that add up to the total:

1. the file written to the watcher reporting it;
2. the watcher's report to the frame's first measured work (the event loop waking up, reading and
   parsing the file);
3. the frame itself: restyle, layout, raster and the present call;
4. the present call returning to the compositor presenting the frame.

All of it is on the QPC clock. The window process prints each frame's watcher stamp, start and end
(through the profiler's frame sink); the compositor's time comes from DXGI desktop duplication, which
stamps every frame it presents. A GDI pixel read was rejected: each read costs one display frame, so
it could not say when the frame was presented. `--write atomic` writes a temporary file and renames
it, as many editors do. The first edit after the window opens is reported apart. `--unchanged`
rewrites the same stylesheet while waiting for a color that never comes: every edit must time out, which
shows the tool does not report a latency for nothing.

`florui-bench edit-latency-ab --a EXE --b EXE [--repeats N] [--rounds N] [--rows N] [--write ...]`
compares two builds: it alternates them (flipping the order each repeat), each measuring its own
window, pools the edits and gives the same 5% and bootstrap-interval verdict as `ab` for every
interval. Run it first with the same build on both sides: with 60 edits a side the noise on the frame
is around 10%, so only a larger change can be told apart.

It needs Windows, a release build with `--features profiling`, and a desktop session nobody is using:
the window is kept on top and one pixel of it is watched, so something covering it, or a second monitor
it straddles, breaks the run (it says so). The pixel is read on the monitor that holds the window.

What it does not see: display scan-out and panel response, and GPU time. The engine's own share is the
headless `css_reload_100_rows` and `css_reload_1k_rows` workloads (parse, install, update, paint after a
stylesheet swap that changes layout); they build the accessibility tree too, which the window does not
unless a screen reader is attached.

## What it does not measure

The other workloads run in a headless window: update, layout, paint and the
accessibility tree, all CPU work. Presentation and GPU time are not included in
them; `edit-latency` above covers presentation to the compositor for a
stylesheet edit only. Results are for the machine in the report; compare
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
- `effects-8d1fd88`: only the `effects_*` workloads, 5 processes, on the commit that adds them (a
  rebase can rewrite its hash, the tree was clean, and the engine is `grow/main` as of the cascade
  style sharing). A 600 x 400 panel costs 4.0 ms opaque and 5.5 ms translucent; with a backdrop blur of
  8 px it costs 32.4 ms (a 200 x 150 panel 8.1 ms against 4.1), with 24 px 62.5 ms, with three
  overlapping panels 34.2 ms, and at a display scale of 2 128 ms against 14.7 translucent; `filter:
  blur(8px)` on the element costs 41.2 ms and the color filter chain 22.8 ms. Between-process
  spread is 4% to 19%, so compare ratios within the report, not a single absolute figure.

- `edit-latency-65e028d`: four `edit-latency` runs (1 and 1,000 rows, in-place and atomic writes, 60
  edits each after a cold one) on a 180 Hz monitor, release with the profiler idle, on the commit that
  adds the tool (a rebase can rewrite its hash; the tree was clean). From the file write
  to the composed frame: 13.7 ms in place and 14.6 ms atomic for one row, 37.0 and 40.3 ms for 1,000
  rows. The frame is 4.4 to 4.5 ms for one row and 28.5 to 28.7 for 1,000; the present call returning
  to composition is 4.9 to 5.9 ms, about one refresh interval (5.56 ms); the file write to the
  watcher's report is 1.3 ms in place and 2.8 to 3.1 ms atomic. One of the 60 in-place edits over
  1,000 rows showed an intermediate color. A save reloads the stylesheet twice, not once: a 50 ms
  sleep in the reload (not committed) added about 100 ms to the total in both write modes. Display
  scan-out is not included.
- `css-reload-65e028d`: the engine's share, headless, 5 processes: 9.8 ms for 100 rows and 49.0 ms for
  1,000 (it builds the accessibility tree, which the window does not without a screen reader).

- `reload-flash-d6470ea`: whether a reload shows a half-read stylesheet or fails to read it. Six cases
  of 250 edits each (one row with 1, 20 and 100 KB of rules, in place and atomic, and 1,000 rows with
  20 KB in place), each on two builds: `current/`, the reload before the change that skips a
  reload whose text is already installed (the window reloaded twice per save), and `deduplicated/`,
  the reload with it, which is what `grow/main` has now. The binaries were built from the tool in this change before it was committed. No edit
  of the 1,506 on either build showed a color that was neither the old nor the new one, and none went to
  another color again after showing the new one; no reload failed, in place or atomic (where the
  first event is a `Remove`). Together with the four earlier 60-edit runs that is one intermediate
  color in 3,252 edits, in a 1,000-row page with a tiny stylesheet, and it was not reproduced; its
  cause is unknown. Reloads per edit: 502 to 518 for 251 edits on `current`, 251 on `deduplicated`.
  The interval from the watcher's report to the frame's first work grows with the stylesheet (one
  row, in place, deduplicated: 3.6 ms at 1 KB, 18.8 at 20 KB, 22.3 at 100 KB), so reading and parsing
  the file is where a large stylesheet's time goes; the profiler has no span for it and these runs do
  not split it further. Median totals, current against deduplicated, without an interval (this is two
  runs, not an `ab`): 1 KB 15.9 and 14.9 ms, 20 KB 35.7 and 31.6, 100 KB 55.6 and 44.9, and 1,000 rows
  with 20 KB 56.6 and 45.9.

Repeat a measurement on a quiet machine before trusting a small difference.
