# Edit to present

- 1 rows, atomic writes, 60 edits measured (0 timed out, 0 showed an intermediate frame)
- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel, 180 Hz
- "Composed" is when the compositor presented the frame (DXGI desktop duplication), on the same QPC clock as the rest. Display scan-out and panel response are not seen.

| Interval | Median (ms) | Spread (MAD) | p95 | Min |
| --- | ---: | ---: | ---: | ---: |
| file write to watcher report | 2.75 | 0.29 | 5.44 | 1.93 |
| watcher report to frame start | 0.88 | 0.40 | 2.32 | 0.33 |
| frame (restyle, layout, raster, present call) | 4.54 | 0.45 | 8.41 | 3.77 |
| present call returned to composed | 5.88 | 1.05 | 8.55 | 2.64 |
| total, file write to composed | 14.58 | 1.48 | 20.91 | 11.94 |

The first edit after the window opened (kept out of the table): 15.07 ms in total, 5.81 ms of it the frame.
