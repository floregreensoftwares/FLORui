# Edit to present

- 1 rows, in-place writes, 250 edits measured (0 timed out, 0 showed an intermediate frame)
- 20 KB of stylesheet; 251 reloads for 251 edits, 0 reload failures; 0 edits went to another color after showing the new one
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.39 | 0.20 | 2.22 | 0.98 |
| watcher report to frame start | 18.79 | 4.20 | 25.34 | 10.43 |
| frame (restyle, layout, raster, present call) | 4.71 | 0.28 | 9.54 | 4.30 |
| present call returned to composed | 6.26 | 1.38 | 9.44 | 3.03 |
| total, file write to composed | 31.62 | 4.38 | 41.74 | 22.05 |

The first edit after the window opened (kept out of the table): 30.39 ms in total, 5.40 ms of it the frame.
