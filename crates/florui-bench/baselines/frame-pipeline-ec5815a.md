# Performance baseline: grow-main-c1db649

- Commit: `ec5815a2a7585d2cec173b5f3aedcec00fb05692`
- Build: release profile, rustc 1.94.0 (4a4ef493e 2026-03-02)
- Profiler: compiled out
- OS: Microsoft Windows [vers�o 10.0.26100.9457]
- CPU: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel (28 logical cores)
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.

| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| update_100_rows | 5 | 500 | 6.91 | 5.85 | 7.43 | 7.0% | 22.6% | 12365 | 4100 KiB | 3.2 MiB |
| update_1k_rows | 5 | 500 | 63.5 | 52.5 | 63.8 | 3.9% | 4.7% | 121323 | 37031 KiB | 22.0 MiB |
| update_10k_rows | 5 | 59 | 714 | 701 | 794 | 2.3% | 1.5% | 1321269 | 382734 KiB | 232.1 MiB |
| frame_100_rows | 5 | 500 | 16.2 | 12.3 | 15.0 | 6.1% | 18.0% | 15490 | 9907 KiB | 6.0 MiB |
| frame_1k_rows | 5 | 500 | 95.1 | 75.3 | 91.1 | 2.6% | 3.0% | 140670 | 47188 KiB | 22.5 MiB |
| frame_10k_rows | 5 | 50 | 2556 | 2528 | 2672 | 1.9% | 2.8% | 1500733 | 438440 KiB | 236.7 MiB |
| signal_update_100_rows | 5 | 500 | 7.03 | 5.69 | 7.06 | 8.1% | 22.7% | 12368 | 4101 KiB | 3.2 MiB |
| signal_update_1k_rows | 5 | 500 | 53.3 | 50.7 | 62.0 | 2.2% | 3.2% | 121326 | 37031 KiB | 22.0 MiB |
| signal_update_10k_rows | 5 | 60 | 726 | 696 | 755 | 2.1% | 2.6% | 1321272 | 382734 KiB | 232.1 MiB |
| signal_frame_1k_rows | 5 | 499 | 94.4 | 75.0 | 93.5 | 3.4% | 1.8% | 140706 | 47239 KiB | 22.5 MiB |
| resize_1k_rows | 5 | 500 | 87.4 | 74.3 | 89.6 | 2.6% | 2.9% | 140401 | 46581 KiB | 22.5 MiB |
| scroll_1k_rows | 5 | 500 | 35.8 | 27.3 | 33.5 | 2.6% | 3.4% | 19363 | 10697 KiB | 22.0 MiB |
| hover_1k_rows | 5 | 500 | 107 | 75.6 | 92.3 | 3.0% | 3.6% | 140745 | 47483 KiB | 22.5 MiB |
| deep_100_levels | 5 | 500 | 7.45 | 5.79 | 6.61 | 11.1% | 20.2% | 4320 | 5995 KiB | 5.1 MiB |
| text_200_paragraphs | 5 | 135 | 292 | 291 | 344 | 1.8% | 4.2% | 107117 | 148379 KiB | 7.3 MiB |
| text_reflow_200_paragraphs | 5 | 136 | 323 | 289 | 353 | 2.5% | 1.4% | 113010 | 148043 KiB | 7.3 MiB |
| clips_shadows_500_cards | 5 | 500 | 64.5 | 37.7 | 49.2 | 5.1% | 2.6% | 43780 | 31207 KiB | 9.6 MiB |
| startup_1k_rows | 5 | 426 | 106 | 93.3 | 114 | 4.8% | 2.6% | 159764 | 51925 KiB | 21.5 MiB |

Cold is the first operation in a fresh process (median over processes); warm is every operation after three discarded warm-up runs, pooled over processes. A `-` for p95 means too few samples for a stable tail. Heap figures come from separate runs with a counting allocator.

## What each workload exercises

- `update_100_rows`: A re-render of 100 rows with nothing changed. Exercises render, arena build, cascade, layout.
- `update_1k_rows`: A re-render of 1,000 rows with nothing changed. Exercises render, arena build, cascade, layout.
- `update_10k_rows`: A re-render of 10,000 rows with nothing changed. Exercises render, arena build, cascade, layout.
- `frame_100_rows`: Update and paint of 100 rows. Exercises update, paint parts, rasterization, accessibility tree.
- `frame_1k_rows`: Update and paint of 1,000 rows. Exercises update, paint parts, rasterization, accessibility tree.
- `frame_10k_rows`: Update and paint of 10,000 rows. Exercises update, paint parts, rasterization, accessibility tree.
- `signal_update_100_rows`: One signal write read by the root, then the update, 100 rows. Exercises signal write, re-render, whole-tree cascade and layout.
- `signal_update_1k_rows`: One signal write read by the root, then the update, 1,000 rows. Exercises signal write, re-render, whole-tree cascade and layout.
- `signal_update_10k_rows`: One signal write read by the root, then the update, 10,000 rows. Exercises signal write, re-render, whole-tree cascade and layout.
- `signal_frame_1k_rows`: A local state change shown on screen: write, update, paint, 1,000 rows. Exercises the whole path from a signal write to a painted frame.
- `resize_1k_rows`: Alternate between two window widths and paint, 1,000 rows. Exercises layout at a new width, paint at a new size.
- `scroll_1k_rows`: One wheel notch over a scroll box of 1,000 rows, then paint. Exercises wheel input, scroll offset update, update, paint with clipping.
- `hover_1k_rows`: Move the pointer to another row and paint, 1,000 rows. Exercises hit test, hover state, restyle of the changed rows, paint.
- `deep_100_levels`: Update and paint of a row nested 100 levels deep. Exercises deep cascade inheritance, deep layout, deep paint.
- `text_200_paragraphs`: Update and paint of 200 wrapped paragraphs with inline spans. Exercises inline shaping, line breaking, text paint.
- `text_reflow_200_paragraphs`: Alternate between two widths so 200 paragraphs reflow, then paint. Exercises line breaking at a new width, text paint.
- `clips_shadows_500_cards`: Update and paint of 500 cards with nested clips, radii and shadows. Exercises clip masks, rounded corners, box-shadow blur.
- `startup_1k_rows`: Create a window with 1,000 rows and paint its first frame. Exercises stylesheet parse, first render, first layout, first paint, font and cache warm-up.
