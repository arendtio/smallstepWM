# Install with pacman

As a user, I want one package built for the current Arch Linux x86_64 system, so that I copy that file to the laptop and install it there with pacman.

## Acceptance

- The delivered artifact is one Arch package for x86_64.
- It is built for the library set of current Arch Linux.
- On the laptop the install step is pacman. No compiler toolchain is required there.
- The package declares its runtime dependencies. Pacman installs those from the official Arch repositories.
