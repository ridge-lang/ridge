# Ridge Examples

Standalone sample programs. Each file is self-contained and runnable
once `ridge` is on your `PATH`:

```sh
ridge run examples/<name>.ridge
```

| File | What it shows |
|------|---------------|
| [`game_of_life.ridge`](game_of_life.ridge) | Conway's Game of Life on a fixed grid. Pure `step` function, records with `{ rows, cols, cells }`, I/O and timing confined to `main`. |
| [`log_analyzer.ridge`](log_analyzer.ridge) | Log parsing with record types and `Result` chaining. Hour-bucket histogram with all I/O at the edges and pure inner aggregation. |
| [`rate_limiter.ridge`](rate_limiter.ridge) | Token-bucket rate limiter as an actor. Five worker actors drive load; a collector actor totals the results. |
| [`url_shortener.ridge`](url_shortener.ridge) | In-memory URL shortener. A `Store` actor owns the code-to-URL map; an HTTP server translates `POST /shorten` and `GET /:code` into actor messages. |

Larger end-to-end programs with their own manifests live under
[`../dogfood/`](../dogfood/).
