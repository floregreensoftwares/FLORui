# Performance baseline: scroll-hover-ebd9729

- Commit: `ebd9729b2b586ee3ba268d0721d9aedef11ae15e`
- Build: release profile, rustc 1.94.0 (4a4ef493e 2026-03-02)
- Profiler: compiled out
- OS: Microsoft Windows [versão 10.0.26100.9457]
- CPU: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel (28 logical cores)
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.

| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| scroll_1k_rows | 8 | 648 | 95.0 | 96.4 | 119 | 4.7% | 6.0% | 125619 | 48315 KiB | 24.3 MiB |
| hover_1k_rows | 8 | 693 | 93.8 | 90.6 | 111 | 4.4% | 5.5% | 118745 | 47280 KiB | 24.3 MiB |

Cold is the first operation in a fresh process (median over processes); warm is every operation after three discarded warm-up runs, pooled over processes. A `-` for p95 means too few samples for a stable tail. Heap figures come from separate runs with a counting allocator.

## What each workload exercises

- `scroll_1k_rows`: One 100 px wheel tick over a scroll box of 1,000 rows, then paint. Exercises wheel input, scroll offset update, repaint-only scroll, paint with clipping.
- `hover_1k_rows`: Move the pointer to another row and paint, 1,000 rows. Exercises hit test, hover state, restyle of the changed rows, paint.
