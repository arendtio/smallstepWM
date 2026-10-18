# Shell without a desktop

As a user, I want a Wayland shell that is itself the desktop environment, so that no separate desktop sits beside the visible content.

## Acceptance

- The shell is a Wayland compositor.
- There is no desktop surface with icons or an empty background.
- Some content is always visible.
- The shell itself is sparing with CPU and memory. Idle does not animate or redraw.
