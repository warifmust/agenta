# Contributing

Thanks for contributing to Agenta.

## Setup

1. Install Rust (stable) and Cargo.
2. Clone the repo and enter it:
   - `git clone https://github.com/warifmust/agenta.git`
   - `cd agenta`
3. Build:
   - `cargo build`

## Before Opening a PR

Run:

- `cargo check`
- `cargo test`

If your change affects CLI behavior, update `README.md` examples.

## Pull Requests

- Keep PRs focused and small.
- Add a clear description of what changed and why.
- Include reproduction/validation steps.

## Commit Style

Use short, direct commit messages, for example:

- `feat: add command trigger mapping`
- `fix: handle long daemon responses in cli`
- `docs: update swagger setup`
