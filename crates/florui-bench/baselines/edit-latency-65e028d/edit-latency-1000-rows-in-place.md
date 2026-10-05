# Edit to present

- 1000 rows, in-place writes, 60 edits measured (0 timed out, 1 showed an intermediate frame)
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.29 | 0.14 | 2.02 | 0.99 |
| watcher report to frame start | 2.12 | 0.61 | 3.11 | 0.55 |
| frame (restyle, layout, raster, present call) | 28.68 | 3.78 | 42.37 | 22.65 |
| present call returned to composed | 5.33 | 1.84 | 9.14 | 2.27 |
| total, file write to composed | 37.00 | 4.20 | 55.64 | 29.30 |

The first edit after the window opened (kept out of the table): 34.51 ms in total, 26.80 ms of it the frame.
