# Edit to present

- 1 rows, in-place writes, 60 edits measured (0 timed out, 0 showed an intermediate frame)
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 1.25 | 0.17 | 2.21 | 0.96 |
| watcher report to frame start | 1.84 | 0.50 | 2.83 | 1.00 |
| frame (restyle, layout, raster, present call) | 4.36 | 0.53 | 7.92 | 3.66 |
| present call returned to composed | 5.74 | 1.08 | 8.43 | 1.69 |
| total, file write to composed | 13.74 | 1.16 | 18.96 | 10.27 |

The first edit after the window opened (kept out of the table): 15.50 ms in total, 8.08 ms of it the frame.
