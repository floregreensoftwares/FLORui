# Edit to present

- 1 rows, atomic writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 1 KB of stylesheet; 518 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 3.63 | 0.72 | 5.41 | 1.94 |
| watcher report to frame start | 2.63 | 0.28 | 3.26 | 1.52 |
| frame (restyle, layout, raster, present call) | 4.15 | 0.22 | 8.28 | 3.75 |
| present call returned to composed | 5.99 | 1.78 | 9.61 | 2.28 |
| total, file write to composed | 17.85 | 1.73 | 21.42 | 11.66 |

The first edit after the window opened (kept out of the table): 18.63 ms in total, 8.23 ms of it the frame.
