# Edit to present

- 1 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 100 KB of stylesheet; 503 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.54 | 0.23 | 2.66 | 1.11 |
| watcher report to frame start | 24.25 | 4.76 | 31.70 | 14.00 |
| frame (restyle, layout, raster, present call) | 21.72 | 2.41 | 34.64 | 18.12 |
| present call returned to composed | 5.35 | 1.50 | 9.00 | 1.92 |
| total, file write to composed | 55.55 | 5.50 | 69.95 | 40.98 |

The first edit after the window opened (kept out of the table): 55.39 ms in total, 26.60 ms of it the frame.
