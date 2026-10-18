# Implementation proposals

Approaches from the concept discussion. Not requirements. Any item can be dropped without invalidating the user stories.

- The compositor draws in software, shows one fullscreen surface, and does not animate.
- The browser stays a separate fullscreen program. The shell owns the strip. Embedding the browser engine does not remove the page's memory cost and is not a goal by itself.
- Firefox with a tightly limited process count is the starting point, because a Rust shell cannot link it as a library, and Chromium is usually heavier on 1 GB. Chromium stays acceptable if it turns out easier to run in this memory.
- Entries that are not selected stay visible in the strip. Their web document is unloaded before the disk takes over. Selecting one again reloads the page. Input in an unloaded entry is not kept.
- The shell paints the start view itself. The first version is a gradient in the scanout buffer. An SVG, PNG, or AVIF decoded at runtime, and the same picture painted by the browser, are not used: the costly part is a live browser process, not the file. A designed picture can later be rasterized once at build time into that same buffer.
- Scan-out uses a DRM dumb buffer mapped with the dumb-buffer map ioctl. The CPU mapping of that buffer's PRIME dma-buf fails on this device class, so the compositor does not use that path.
- The voice client and the MCP server live in the shell process.
- The strip expands only after a short delay, so the pointer passing across it does not open it.
- While a video is playing, the strip is hidden. It returns when the pointer touches the left edge.
- Fixed icons for network, sound, power, and voice sit at the bottom of the strip. Only the middle section scrolls.
- The Fn keys for brightness, volume, mute, sleep, display, and touchpad stay a direct path and work without the API.
- Shell commands that change the system or reach widely need a visible boundary before they run.
- The first build is the strip, the shell-drawn start view, and Firefox as one fullscreen window, plus the package and the SDDM session. Voice, MCP, and mouse-driven Wi-Fi come after that. In that first build Firefox keeps its own toolbar, and the strip lists the browser window rather than each page.
- The package is built with makepkg against current Arch, in a container or chroot, and handed over as one `.pkg.tar.zst` file. The laptop installs it with `pacman -U`. Publishing to the AUR is not part of that handoff.
