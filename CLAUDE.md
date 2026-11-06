# Sample Manager

## Workflow rules

- **Build after every code change, but never kill or launch the app.** The user keeps the app running while they work. At the end of each round where code changed, just build:
  ```bash
  cargo build --release
  ```
  If the build fails, fix it before ending the round. Never run `pkill`/`kill` on `sample_manager` and don't start it yourself. When a newer build lands, the running app shows a **New build: relaunch** button in its toolbar, and the user relaunches when they're ready. Tell them the build is ready to relaunch.
- **Don't write tests.** Verify by building instead (the user runs the app). When unsure whether a change works in edge cases, tell the user which edge conditions to try and what symptoms to look for.
