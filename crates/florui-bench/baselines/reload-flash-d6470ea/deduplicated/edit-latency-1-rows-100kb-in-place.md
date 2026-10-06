# Edit to present

- 1 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 100 KB of stylesheet; 251 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.46 | 0.18 | 2.51 | 1.09 |
| watcher report to frame start | 22.27 | 4.00 | 32.02 | 14.45 |
| frame (restyle, layout, raster, present call) | 12.00 | 1.12 | 24.32 | 10.08 |
| present call returned to composed | 5.73 | 1.51 | 8.81 | 2.26 |
| total, file write to composed | 44.85 | 5.17 | 61.27 | 30.20 |

The first edit after the window opened (kept out of the table): 42.13 ms in total, 13.12 ms of it the frame.
