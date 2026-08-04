## Highlights

- Makes the sandbox a first-class middleware so capability commands, lifecycle hooks, frontend
  contributions, and shutdown all use the same ordered middleware pipeline.
- Requires each middleware that registers tools to render begin, success, and error lifecycle
  events, preventing capability output from disappearing in remote frontends.
- Keeps coding presentation with the coding tools, adds frontend-neutral subagent and recurring-task
  rendering, and moves replay metadata into the protocol owner.
- Adds reusable denied-read, isolated-home, and read-only argv execution to the local sandbox.

## Contract

- This is the current framework contract only. Removed presentation and sandbox ownership paths
  have no aliases, adapters, or fallback dispatch.
