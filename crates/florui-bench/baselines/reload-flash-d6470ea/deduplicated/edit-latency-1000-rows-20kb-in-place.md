# Edit to present

- 1000 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 20 KB of stylesheet; 251 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.41 | 0.22 | 2.21 | 0.98 |
| watcher report to frame start | 20.78 | 3.21 | 25.80 | 11.97 |
| frame (restyle, layout, raster, present call) | 16.94 | 1.38 | 31.17 | 14.45 |
| present call returned to composed | 5.00 | 1.19 | 9.33 | 2.31 |
| total, file write to composed | 45.90 | 4.72 | 59.11 | 33.74 |

The first edit after the window opened (kept out of the table): 51.35 ms in total, 19.94 ms of it the frame.
