# Working rules for this repository

## Do not run the test suite locally

Tests run in CI, not here. Push the branch, open the pull request, and read
the result and the logs from the GitHub Actions run.

This is not a preference about tidiness. A local `cargo test --workspace`
takes minutes of a session that is paid for by the token, runs against one
platform, and proves nothing about the three-platform matrix, the living-PRD
check, or the end-to-end suite that installs the published artifact — all of
which CI runs anyway on the same commit. Running it here spends time twice
and answers a narrower question.

So:

- **Do** run `cargo fmt --all` and `cargo clippy --workspace --all-targets
  --locked` before committing. They are seconds, and a formatting or lint
  failure is a wasted CI cycle rather than information.
- **Do** run a single focused test when a specific behaviour is genuinely in
  doubt and the answer changes what gets written next.
- **Do not** run `cargo test --workspace`, the end-to-end script, or any
  broad suite locally. Open the PR and read the pipeline.

When reporting a result, read the actual run on the current head — not an
earlier run on a superseded commit.
