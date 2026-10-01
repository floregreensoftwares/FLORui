# Performance baseline: effects

- Commit: `8d1fd88b0713159322aeada0153166100ce7d802`
- Build: release profile, rustc 1.94.0 (4a4ef493e 2026-03-02)
- Profiler: compiled out
- OS: Microsoft Windows [vers�o 10.0.26100.9457]
- CPU: Intel64 Family 6 Model 79 Stepping 1, GenuineIntel (28 logical cores)
- Power: GUID do Esquema de Energia: 381b4222-f694-41f0-9685-ff5bb260df2e  (Equilibrado)
- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.

| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| effects_opaque_large | 5 | 500 | 4.09 | 3.99 | 4.90 | 5.8% | 17.2% | 2421 | 4912 KiB | 6.3 MiB |
| effects_translucent_large | 5 | 500 | 5.27 | 5.50 | 6.94 | 8.8% | 15.1% | 2421 | 4912 KiB | 6.3 MiB |
| effects_translucent_small | 5 | 500 | 4.73 | 4.07 | 5.45 | 4.3% | 13.7% | 2421 | 4912 KiB | 6.3 MiB |
| effects_backdrop_blur_8_small | 5 | 500 | 8.61 | 8.11 | 9.32 | 8.8% | 19.3% | 2442 | 6085 KiB | 10.4 MiB |
| effects_backdrop_blur_8_large | 5 | 500 | 36.3 | 32.4 | 43.2 | 3.7% | 14.8% | 2442 | 14289 KiB | 10.4 MiB |
| effects_backdrop_blur_24_large | 5 | 500 | 61.8 | 62.5 | 77.5 | 1.9% | 16.1% | 2442 | 14290 KiB | 10.4 MiB |
| effects_translucent_overlap3 | 5 | 500 | 5.16 | 5.26 | 6.21 | 5.9% | 10.3% | 2467 | 4917 KiB | 6.3 MiB |
| effects_backdrop_blur_8_overlap3 | 5 | 500 | 37.3 | 34.2 | 41.8 | 5.6% | 5.5% | 2530 | 13922 KiB | 10.4 MiB |
| effects_translucent_large_dpr2 | 5 | 500 | 15.2 | 14.7 | 16.6 | 6.9% | 11.7% | 2421 | 16162 KiB | 17.3 MiB |
| effects_backdrop_blur_8_large_dpr2 | 5 | 303 | 125 | 128 | 153 | 3.4% | 8.3% | 2442 | 53666 KiB | 26.9 MiB |
| effects_filter_blur_8 | 5 | 500 | 40.4 | 41.2 | 49.1 | 2.5% | 3.8% | 2443 | 16340 KiB | 10.4 MiB |
| effects_filter_chain | 5 | 500 | 22.3 | 22.8 | 30.8 | 3.4% | 4.7% | 2423 | 5857 KiB | 10.4 MiB |

Cold is the first operation in a fresh process (median over processes); warm is every operation after three discarded warm-up runs, pooled over processes. A `-` for p95 means too few samples for a stable tail. Heap figures come from separate runs with a counting allocator.

## What each workload exercises

- `effects_opaque_large`: Paint of busy content with one opaque 600 x 400 panel over it. Exercises the baseline: covering the same pixels with an opaque fill.
- `effects_translucent_large`: The same panel translucent, with no filter. Exercises alpha blending over busy content; the equivalent work without an effect.
- `effects_translucent_small`: A translucent 200 x 150 panel with no filter. Exercises the baseline for the small backdrop-filter panel.
- `effects_backdrop_blur_8_small`: A translucent 200 x 150 panel with backdrop-filter: blur(8px). Exercises sampling and blurring the content behind a small area.
- `effects_backdrop_blur_8_large`: A translucent 600 x 400 panel with backdrop-filter: blur(8px). Exercises the same blur over a larger area.
- `effects_backdrop_blur_24_large`: A translucent 600 x 400 panel with backdrop-filter: blur(24px). Exercises a larger blur radius over the same area.
- `effects_translucent_overlap3`: Three overlapping translucent 320 x 240 panels with no filter. Exercises the baseline for overlapping backdrop-filter panels.
- `effects_backdrop_blur_8_overlap3`: Three overlapping translucent panels, each with backdrop-filter: blur(8px). Exercises each panel blurs the content and the panels beneath it.
- `effects_translucent_large_dpr2`: The translucent 600 x 400 panel at a display scale of 2. Exercises the baseline at 1600 x 1200 physical pixels.
- `effects_backdrop_blur_8_large_dpr2`: The 600 x 400 backdrop-filter: blur(8px) panel at a display scale of 2. Exercises four times the pixels for the same effect.
- `effects_filter_blur_8`: A translucent 600 x 400 panel and its content with filter: blur(8px). Exercises an element rendered to an intermediate surface and blurred.
- `effects_filter_chain`: The same panel with filter: brightness() contrast() saturate(). Exercises an ordered chain of color filters on an intermediate surface.
