# CI workflow (staged here — one move to activate)

`ci.yml` is a ready-to-use GitHub Actions workflow. It lives in `ci/` instead of
`.github/workflows/` because the automation credential that created it lacked
GitHub's `workflow` scope, so it couldn't be committed to the workflows path
directly. Your normal login has that scope.

**Activate it** (from a clone whose credentials have the `workflow` scope):

```bash
git mv ci/ci.yml .github/workflows/ci.yml
git commit -m "ci: activate GitHub Actions workflow"
git push
```

(Or paste `ci.yml` into the GitHub UI: repo → **Actions** → **New workflow** →
"set up a workflow yourself".)

**What it does** — split by what each OS is actually needed for:

- **`linux` job** — the non-overlay Rust tests (`cargo test --features llm`; the
  AXUI/AppKit overlay modules are `cfg`'d out off-macOS) plus the Python / JS /
  shell checks. Covers ~45 of the ~64 tests and the whole model pipeline. **No
  macOS.**
- **`macos-overlay` job** — the only place the macOS Accessibility/AppKit
  overlay code compiles, so it runs the full `--features llm,overlay` suite.
  Optional: delete this job to skip macOS minutes — the overlay is still covered
  locally via `./scripts/test.sh`.

> Note: the `linux` job's first run is slow (it builds llama.cpp from source via
> the `llm` feature); `Swatinem/rust-cache` makes subsequent runs fast. This
> workflow hasn't been executed in CI yet — the first run is its real test.
