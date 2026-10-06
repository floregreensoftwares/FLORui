# Edit to present

- 1 rows, atomic writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 20 KB of stylesheet; 517 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 3.21 | 0.59 | 5.64 | 2.05 |
| watcher report to frame start | 18.86 | 2.70 | 23.34 | 10.67 |
| frame (restyle, layout, raster, present call) | 7.09 | 0.35 | 14.86 | 6.50 |
| present call returned to composed | 6.27 | 1.58 | 9.32 | 2.38 |
| total, file write to composed | 35.97 | 3.62 | 44.31 | 23.72 |

The first edit after the window opened (kept out of the table): 30.76 ms in total, 8.05 ms of it the frame.
