# Profiler overhead

What the profiler costs, measured on the engine as of `grow/main` at 7afd734 plus the
benchmark tool changes of the same commit.

- Machine: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 28 logical cores, Microsoft Windows [vers�o 10.0.26100.9457]
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Toolchain: rustc 1.94.0 (4a4ef493e 2026-03-02), release profile
- Headless window: update, layout, paint and the accessibility tree; no GPU and no presentation.
  Presentation is covered by the GPU spans' own timings, not by this report.

## How

Two release binaries of the same source: **off** is built without the `profiling` feature, so
the profiler does not exist in it (every span and counter compiles away); **on** is built with it.
`florui-bench ab` alternates the two builds round by round (5 rounds, one process per workload
per round, 5 rounds' warm samples pooled), and the profiler mode of **on** is chosen per
comparison: **idle** (built in, never started), **summary** (started, a summary per frame) and
**detail** (started with every span kept). Seven workloads: updates and frames of 100 and 1,000 rows,
a signal write painted, hover, 200 wrapped paragraphs, and 500 cards with clips and shadows.
A change counts when it exceeds 5% and its 95% bootstrap interval excludes zero.

## One span and one counter

| Profiler | One span (ns) | One counter update (ns) |
| --- | ---: | ---: |
| idle | 10.1 | 2.8 |
| summary | 145.1 | 4.8 |
| detail | 170.1 | 6.4 |

Nanoseconds per call, the cost of an empty loop taken off. Detail mode finishes a frame every 1,000
spans, so that cost is amortized in. A frame has about 15 spans, so in summary mode they cost
about 2 microseconds, roughly 0.03% of a 6.5 ms update and far less of a 140 ms frame; counters run
once per node painted, about 5 ns each.

## Per workload

### Idle (built in, not started) against compiled out

| Workload | Baseline (ms) | Candidate (ms) | Change | 95% interval | Verdict | Cold change |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| update_100_rows | 6.64 | 6.58 | -1.0% | -2.5% to +0.4% | no difference | -5.8% |
| frame_1k_rows | 140 | 140 | +0.3% | -1.2% to +2.0% | no difference | +5.8% |
| signal_update_100_rows | 6.47 | 6.66 | +2.9% | +1.6% to +4.1% | no difference | -0.5% |
| signal_frame_1k_rows | 142 | 139 | -2.4% | -3.8% to -0.6% | no difference | +0.7% |
| hover_1k_rows | 139 | 146 | +4.8% | +3.2% to +6.5% | no difference | +3.2% |
| text_200_paragraphs | 311 | 316 | +1.7% | +0.7% to +4.2% | no difference | -3.4% |
| clips_shadows_500_cards | 75.5 | 76.6 | +1.4% | +0.6% to +2.3% | no difference | +0.8% |

### Summary

| Workload | Baseline (ms) | Candidate (ms) | Change | 95% interval | Verdict | Cold change |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| update_100_rows | 6.38 | 6.65 | +4.3% | +3.3% to +5.3% | no difference | +5.1% |
| frame_1k_rows | 139 | 139 | +0.3% | -1.0% to +1.2% | no difference | -6.7% |
| signal_update_100_rows | 6.67 | 6.71 | +0.6% | -0.4% to +1.8% | no difference | -4.2% |
| signal_frame_1k_rows | 140 | 141 | +0.7% | -0.6% to +1.7% | no difference | +4.2% |
| hover_1k_rows | 143 | 142 | -0.3% | -1.7% to +0.9% | no difference | -1.3% |
| text_200_paragraphs | 313 | 313 | -0.0% | -1.1% to +1.2% | no difference | +3.9% |
| clips_shadows_500_cards | 75.5 | 77.7 | +2.9% | +1.8% to +3.7% | no difference | +8.8% |

### Detail

| Workload | Baseline (ms) | Candidate (ms) | Change | 95% interval | Verdict | Cold change |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| update_100_rows | 6.30 | 6.29 | -0.2% | -1.4% to +1.2% | no difference | -2.1% |
| frame_1k_rows | 132 | 131 | -0.2% | -0.9% to +0.5% | no difference | -9.4% |
| signal_update_100_rows | 6.35 | 6.17 | -2.8% | -4.4% to -1.6% | no difference | +1.8% |
| signal_frame_1k_rows | 131 | 133 | +1.7% | +1.1% to +2.4% | no difference | +0.7% |
| hover_1k_rows | 132 | 133 | +0.9% | -0.0% to +1.9% | no difference | -2.2% |
| text_200_paragraphs | 305 | 305 | +0.0% | -1.0% to +0.9% | no difference | +3.5% |
| clips_shadows_500_cards | 73.4 | 74.6 | +1.7% | +1.3% to +2.1% | no difference | -0.4% |

### One result repeated

The first idle table shows `hover_1k_rows` at +4.8% (interval +3.2% to +6.5%), the highest of
all. The other two modes put the same workload at -0.3% and +0.9%, so it was repeated with 10
rounds:

| Workload | Baseline (ms) | Candidate (ms) | Change | 95% interval | Verdict | Cold change |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| hover_1k_rows | 140 | 139 | -0.5% | -0.9% to +0.0% | no difference | +6.4% |

## Reading it

- No workload in any mode reached the 5% threshold, and the one close to it did not repeat: with
  10 rounds the same comparison is -0.5%.
- The primitives bound what is real: about 15 spans and one counter update per node painted add up
  to a few microseconds, and tens of microseconds for thousands of nodes, on frames of several
  milliseconds or more. That is below what
  this method can resolve (differences under about 3% drift with the machine), so the honest claim
  is "no measurable overhead at the resolution of these runs", not "zero".
- Absolute times drift between the three tables (for example `frame_1k_rows` baseline 140, 139 and
  132 ms) because they ran at different moments on a machine shared with other work; each table
  compares builds within itself, alternating, which is what makes the comparison valid.

## Not covered here

- Edit-to-present latency (a CSS edit through hot reload to a presented frame) needs a real window
  and a file watcher; it is measured separately.
- GPU execution time is not measured at all; the profiler reports CPU-side submit and present times
  and says GPU time is unavailable rather than estimating it.
- Debug builds always have the profiler available and run the engine unoptimized, so their numbers
  say nothing about release overhead.
