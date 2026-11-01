# Sample Manager

## Workflow rules

- **Relaunch after every code change.** At the end of each round where code changed, build, kill the running app, and start the new build so the user can try it right away:
  ```bash
  cargo build --release && { pkill -x sample_manager; sleep 0.5; (./target/release/sample_manager >/dev/null 2>&1 &); }
  ```
  Run the launch in the background so it doesn't block. If the build fails, fix it before ending the round (leave the old app running in the meantime).
- **Don't write tests.** Verify by building and running the app instead. When unsure whether a change works in edge cases, tell the user which edge conditions to try and what symptoms to look for.
