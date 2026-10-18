# Voice as a toggle

As a user, I want to turn the voice session on and off with F1, so that I can speak without holding the key.

## Acceptance

- One press of F1 turns the session on. The next press turns it off.
- The key does not have to stay held.
- A held key triggers the switch only once.
- While the session is on, I can speak several utterances in a row. The Realtime API separates them.
- There is no wake word.
- F1 does not reach the window under the shell.
- While audio can leave the machine, the shell shows that.
- Off means no audio goes to the API anymore.
- Without a connection the session cannot be turned on, and the shell says why.
- The same switch sits visibly in the strip and works with the mouse.
