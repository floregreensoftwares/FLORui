# Edit to present

- 1 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 1 KB of stylesheet; 251 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.36 | 0.20 | 2.13 | 0.99 |
| watcher report to frame start | 3.64 | 0.52 | 4.41 | 1.45 |
| frame (restyle, layout, raster, present call) | 3.28 | 0.19 | 6.26 | 2.92 |
| present call returned to composed | 6.26 | 1.21 | 9.47 | 2.33 |
| total, file write to composed | 14.86 | 1.52 | 19.41 | 10.44 |

The first edit after the window opened (kept out of the table): 12.63 ms in total, 4.26 ms of it the frame.
