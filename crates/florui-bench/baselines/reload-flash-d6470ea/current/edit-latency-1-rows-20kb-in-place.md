# Edit to present

- 1 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 20 KB of stylesheet; 502 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.35 | 0.19 | 2.20 | 0.97 |
| watcher report to frame start | 20.00 | 3.44 | 25.11 | 10.36 |
| frame (restyle, layout, raster, present call) | 7.34 | 0.63 | 15.02 | 6.55 |
| present call returned to composed | 6.78 | 1.45 | 9.55 | 1.48 |
| total, file write to composed | 35.73 | 4.15 | 45.93 | 23.12 |

The first edit after the window opened (kept out of the table): 35.53 ms in total, 8.59 ms of it the frame.
