# Edit to present

- 1 rows, atomic writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 1 KB of stylesheet; 251 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 3.09 | 0.57 | 4.99 | 1.89 |
| watcher report to frame start | 2.57 | 0.30 | 3.35 | 1.38 |
| frame (restyle, layout, raster, present call) | 3.25 | 0.15 | 6.26 | 2.99 |
| present call returned to composed | 5.45 | 1.42 | 9.30 | 1.72 |
| total, file write to composed | 15.02 | 2.34 | 20.28 | 10.80 |

The first edit after the window opened (kept out of the table): 17.46 ms in total, 3.59 ms of it the frame.
