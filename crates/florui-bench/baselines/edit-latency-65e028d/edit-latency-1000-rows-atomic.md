# Edit to present

- 1000 rows, atomic writes, 60 edits measured (0 timed out, 0 showed an intermediate frame)
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 3.13 | 0.61 | 5.77 | 2.03 |
| watcher report to frame start | 0.82 | 0.36 | 1.93 | 0.33 |
| frame (restyle, layout, raster, present call) | 28.51 | 2.26 | 42.83 | 21.12 |
| present call returned to composed | 4.88 | 1.54 | 9.08 | 1.87 |
| total, file write to composed | 40.26 | 5.02 | 52.83 | 27.32 |

The first edit after the window opened (kept out of the table): 45.36 ms in total, 32.46 ms of it the frame.
