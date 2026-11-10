# Sample Manager

## Finding things

Read [MAP.md](MAP.md) first when a request names a concept, a button or a panel. It covers the vocabulary ("device" and "+Drive" mean an Elektron sampler), which control lives in which function, where styling is set, and where the job queues are. Keep it up to date when you move or rename something it points to.

## Workflow rules

- **Build after every code change, but never kill or launch the app.** The user keeps the app running while they work. At the end of each round where code changed, just build:
  ```bash
  cargo build --release
  ```
  If the build fails, fix it before ending the round. Never run `pkill`/`kill` on `sample_manager` and don't start it yourself. When a newer build lands, the running app shows a **New build: relaunch** button in its toolbar, and the user relaunches when they're ready. Tell them the build is ready to relaunch.
- **Don't write tests.** Verify by building instead (the user runs the app). When unsure whether a change works in edge cases, tell the user which edge conditions to try and what symptoms to look for.
