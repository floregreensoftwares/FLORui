# Performance baseline: css-reload-65e028d

- Commit: `65e028d465d0d5bb3212eb22c3e0bef3e76608eb`
- Build: release profile, rustc 1.94.0 (4a4ef493e 2026-03-02)
- Profiler: idle
- OS: Microsoft Windows [versão 10.0.26100.9457]
- CPU: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel (28 logical cores)
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.

| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| css_reload_100_rows | 5 | 500 | 11.8 | 9.78 | 12.1 | 4.2% | 4.6% | 10569 | 8427 KiB | 7.3 MiB |
| css_reload_1k_rows | 5 | 500 | 56.6 | 49.0 | 59.8 | 2.9% | 2.3% | 96123 | 34282 KiB | 18.5 MiB |

Cold is the first operation in a fresh process (median over processes); warm is every operation after three discarded warm-up runs, pooled over processes. A `-` for p95 means too few samples for a stable tail. Heap figures come from separate runs with a counting allocator.

## What each workload exercises

- `css_reload_100_rows`: A stylesheet swap that changes layout, then paint, 100 rows. Exercises stylesheet parse, restyle, layout, paint.
- `css_reload_1k_rows`: A stylesheet swap that changes layout, then paint, 1,000 rows. Exercises stylesheet parse, restyle, layout, paint.
