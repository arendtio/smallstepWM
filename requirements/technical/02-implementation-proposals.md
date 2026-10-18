# Implementation proposals

Approaches from the concept discussion. Not requirements. Any item can be dropped without invalidating the user stories.

- The compositor draws in software, shows one fullscreen surface, and does not animate.
- The browser stays a separate fullscreen program. The shell owns the strip. Embedding the browser engine does not remove the page's memory cost and is not a goal by itself.
- Firefox with a tightly limited process count is the starting point, because a Rust shell cannot link it as a library, and Chromium is usually heavier on 1 GB. Chromium stays acceptable if it turns out easier to run in this memory.
- Entries that are not selected stay visible in the strip. Their web document is unloaded before the disk takes over. Selecting one again reloads the page. Input in an unloaded entry is not kept.
- The shell draws the start image without starting the browser for it.
- The voice client and the MCP server live in the shell process.
- The strip expands only after a short delay, so the pointer passing across it does not open it.
- While a video is playing, the strip is hidden. It returns when the pointer touches the left edge.
- Fixed icons for network, sound, power, and voice sit at the bottom of the strip. Only the middle section scrolls.
- The Fn keys for brightness, volume, mute, sleep, display, and touchpad stay a direct path and work without the API.
- Shell commands that change the system or reach widely need a visible boundary before they run.
- A buildable order is: strip, start image, and Wi-Fi with the mouse; then the browser as fullscreen; then the voice session; then the remaining tools.
