# Presenting a frame

- release on Microsoft Windows [versão 10.0.26100.9457], Intel64 Family 6 Model 79 Stepping 1, GenuineIntel
- A window of the size shown, repainted every refresh by an animation; medians over the frames after a warm-up, in milliseconds. `acquire` waits for the display, so it is not work; `upload` and `submit` are the CPU's.

| Window | MP | Frames | upload | acquire | submit | flip | present | raster | update |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 800x600 | 0.48 | 400 | 0.27 | 0.01 | 0.22 | 0.23 | 0.75 | 0.71 | 0.82 |
| 1920x1080 | 2.07 | 400 | 0.75 | 0.01 | 0.22 | 0.24 | 1.27 | 2.70 | 0.84 |
| 3840x2160 | 8.29 | 400 | 3.88 | 0.02 | 0.30 | 0.30 | 4.58 | 12.67 | 1.02 |
