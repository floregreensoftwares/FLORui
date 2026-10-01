# Performance baseline: collections

- Commit: `af6ab47089a46bd6ec2524858e6f6afc87b327d3` (with uncommitted changes)
- Build: release profile, rustc 1.94.0 (4a4ef493e 2026-03-02)
- Profiler: compiled out
- OS: Microsoft Windows [vers�o 10.0.26100.9457]
- CPU: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel (28 logical cores)
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.

| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| virtual_list_1k_fixed | 5 | 500 | 11.5 | 11.7 | 14.6 | 2.4% | 3.3% | 5517 | 6962 KiB | 6.7 MiB |
| virtual_list_10k_fixed | 5 | 500 | 11.4 | 11.7 | 14.4 | 2.7% | 9.8% | 5517 | 6963 KiB | 6.9 MiB |
| virtual_list_100k_fixed | 5 | 500 | 11.4 | 11.7 | 14.8 | 3.3% | 19.9% | 5517 | 6963 KiB | 9.8 MiB |
| virtual_list_10k_variable | 5 | 500 | 24.8 | 22.3 | 27.4 | 5.0% | 7.4% | 12271 | 12616 KiB | 6.8 MiB |
| virtual_list_jump_10k_variable | 5 | 500 | 23.6 | 22.5 | 27.8 | 5.1% | 5.9% | 12901 | 12714 KiB | 6.8 MiB |
| mount_unmount_300_components | 5 | 500 | 22.6 | 21.5 | 26.3 | 4.7% | 4.0% | 30350 | 14732 KiB | 7.7 MiB |

Cold is the first operation in a fresh process (median over processes); warm is every operation after three discarded warm-up runs, pooled over processes. A `-` for p95 means too few samples for a stable tail. Heap figures come from separate runs with a counting allocator.

## What each workload exercises

- `virtual_list_1k_fixed`: One 100 px wheel tick over a virtualized list of 1,000 fixed-height rows, then paint. Exercises wheel input, mounting and unmounting the rows that enter and leave, keyed row scopes, paint.
- `virtual_list_10k_fixed`: The same list over 10,000 items. Exercises as the 1,000-item list; a difference between the two is the cost of the data size.
- `virtual_list_100k_fixed`: The same list over 100,000 items. Exercises as the 1,000-item list; a difference between the two is the cost of the data size.
- `virtual_list_10k_variable`: One 100 px wheel tick over a virtualized list of 10,000 rows whose height depends on their text, then paint. Exercises as the fixed list, plus measuring rows after they mount and correcting the estimates.
- `virtual_list_jump_10k_variable`: Jump to an item thousands of rows away in a list of 10,000 variable rows, then paint. Exercises scroll-to-item, a viewport of rows that were never measured, estimate corrections.
- `mount_unmount_300_components`: Unmount 300 components that each keep a signal, a memo and an effect, mount them again, then paint. Exercises mounting and disposing keyed component scopes, hooks, effect cleanups, render of the rows.
