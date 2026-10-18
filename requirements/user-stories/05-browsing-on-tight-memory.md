# Browsing on tight memory

As a user, I want to browse on a laptop with 1 GB of RAM and no GPU acceleration, without the shell and the browser each keeping the same chrome in memory.

## Acceptance

- Browsing is the intended web activity.
- Pages appear in the shell's strip. There is no second strip for the same pages.
- Video is part of browsing. On a machine without GPU acceleration it is the hard case.
- 1 GB is the memory target for the base system, the shell, and browsing. RAM will not be upgraded.
- Spilling onto slow swap counts as a failure, not as a planned state.
