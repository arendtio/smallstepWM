# Constraints

Derived from the target device. No model name, no individual parts.

- The machine has 1 GB of RAM. It will not be upgraded.
- Usable GPU acceleration is not available. Drawing and video decode run in software. The CPU is tight for that.
- The display is short, on the order of 1024×600.
- The base system already uses about 180 MB before this shell.
- There is a large swap area on a slow disk. Using it means the machine visibly stalls.
- The usual network path is wireless. Without it the voice API is unreachable.
- The installed system is current Arch Linux, x86_64, and already starts sessions through SDDM.
- CPU and memory are both scarce. The shell stays cheap while it is running and while it is idle.
