# Edit to present

- 1 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 1 KB of stylesheet; 502 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.29 | 0.15 | 2.10 | 0.96 |
| watcher report to frame start | 3.58 | 0.48 | 4.44 | 1.84 |
| frame (restyle, layout, raster, present call) | 4.18 | 0.17 | 8.26 | 3.87 |
| present call returned to composed | 6.32 | 1.50 | 9.93 | 2.60 |
| total, file write to composed | 15.91 | 1.91 | 21.58 | 11.07 |

The first edit after the window opened (kept out of the table): 31.64 ms in total, 5.02 ms of it the frame.
