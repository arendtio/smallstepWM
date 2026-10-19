# Requirements

What the shell should do, kept apart from how a particular version is built.

- `user-stories/` records the requirements.
- `technical/01-constraints.md` records the device class, without naming a specific model.
- `technical/02-implementation-proposals.md` collects approaches. These are not decisions.
- `technical/03-open-points.md` lists what still has to be chosen.

A user story changes when the requirement changes. A proposal can be dropped without making a story wrong.

The current package covers the shell, the left strip, the start view, Firefox pages in that strip, power controls, mouse-driven Wi-Fi, the voice toggle, the in-process tools, hardware OpenGL ES when a real EGL device is present, the Arch package, and the SDDM session.
