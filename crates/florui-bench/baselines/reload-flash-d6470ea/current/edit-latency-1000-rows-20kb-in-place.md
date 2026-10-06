# Edit to present

- 1000 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 20 KB of stylesheet; 505 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.49 | 0.27 | 2.32 | 0.96 |
| watcher report to frame start | 20.11 | 3.53 | 25.97 | 11.39 |
| frame (restyle, layout, raster, present call) | 27.75 | 3.26 | 42.58 | 22.77 |
| present call returned to composed | 5.59 | 1.52 | 9.36 | 1.82 |
| total, file write to composed | 56.55 | 4.94 | 73.99 | 39.68 |

The first edit after the window opened (kept out of the table): 49.56 ms in total, 31.78 ms of it the frame.
