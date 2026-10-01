# Performance baseline: grow-main-ac1e72b

- Commit: `ac1e72b4c7bcab34922a64741cdae894b1761cbe` (with uncommitted changes)
- Build: release profile, rustc 1.94.0 (4a4ef493e 2026-03-02)
- OS: Microsoft Windows [versão 10.0.26100.9457]
- CPU: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel (28 logical cores)
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.

| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| update_100_rows | 5 | 500 | 9.17 | 8.78 | 11.1 | 5.4% | 11.5% | 16773 | 4606 KiB | 3.5 MiB |
| update_1k_rows | 5 | 448 | 96.2 | 89.4 | 103 | 7.1% | 17.7% | 165331 | 42083 KiB | 25.1 MiB |
| update_10k_rows | 5 | 50 | 1519 | 1493 | 1596 | 1.9% | 6.0% | 1650394 | 416628 KiB | 253.8 MiB |
| frame_100_rows | 5 | 500 | 20.4 | 19.2 | 22.6 | 4.1% | 8.0% | 25056 | 12072 KiB | 6.0 MiB |
| frame_1k_rows | 5 | 240 | 181 | 166 | 194 | 4.8% | 9.2% | 246134 | 74842 KiB | 25.6 MiB |
| frame_10k_rows | 5 | 50 | 4040 | 3916 | 4248 | 2.3% | 5.7% | 2458815 | 725604 KiB | 259.5 MiB |
| signal_update_100_rows | 5 | 500 | 8.94 | 8.45 | 10.2 | 3.5% | 4.7% | 16776 | 4606 KiB | 3.5 MiB |
| signal_update_1k_rows | 5 | 448 | 88.9 | 86.1 | 107 | 3.9% | 6.0% | 165334 | 42084 KiB | 25.1 MiB |
| signal_update_10k_rows | 5 | 50 | 1529 | 1535 | 1726 | 2.9% | 5.8% | 1650398 | 416629 KiB | 253.8 MiB |
| signal_frame_1k_rows | 5 | 238 | 165 | 164 | 198 | 3.8% | 7.7% | 246170 | 74891 KiB | 25.6 MiB |
| resize_1k_rows | 5 | 239 | 164 | 160 | 201 | 3.7% | 6.9% | 246095 | 74303 KiB | 25.6 MiB |
| scroll_1k_rows | 5 | 84 | 539 | 475 | 578 | 2.5% | 3.2% | 98813 | 1461229 KiB | 25.1 MiB |
| hover_1k_rows | 5 | 230 | 204 | 166 | 220 | 5.0% | 18.6% | 247210 | 75325 KiB | 25.6 MiB |
| deep_100_levels | 5 | 500 | 7.71 | 6.43 | 7.29 | 8.7% | 16.2% | 5169 | 6035 KiB | 5.1 MiB |
| text_200_paragraphs | 5 | 125 | 312 | 308 | 392 | 3.2% | 4.7% | 111925 | 148518 KiB | 7.3 MiB |
| text_reflow_200_paragraphs | 5 | 132 | 316 | 301 | 368 | 3.5% | 6.0% | 118044 | 148169 KiB | 7.3 MiB |
| clips_shadows_500_cards | 5 | 50 | 1131 | 1139 | 1319 | 3.8% | 9.6% | 93183 | 418967 KiB | 9.4 MiB |
| startup_1k_rows | 5 | 233 | 193 | 172 | 202 | 6.7% | 13.3% | 250204 | 76876 KiB | 22.9 MiB |

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
