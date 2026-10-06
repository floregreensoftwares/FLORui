# Edit to present

- 1 rows, atomic writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 20 KB of stylesheet; 251 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 3.26 | 0.62 | 5.21 | 2.09 |
| watcher report to frame start | 20.15 | 2.94 | 24.57 | 10.13 |
| frame (restyle, layout, raster, present call) | 4.81 | 0.38 | 9.65 | 4.21 |
| present call returned to composed | 5.21 | 1.51 | 8.98 | 1.94 |
| total, file write to composed | 34.60 | 3.84 | 41.08 | 21.34 |

The first edit after the window opened (kept out of the table): 41.63 ms in total, 8.69 ms of it the frame.
