# Screenshots

`mise run screenshots vX.Y.Z` renders every image here from the code: the browser app in Chrome and the terminal
client from its own frame, each drawing a fixed simulated run, so the same code in the same browser gives the same
pixels. `screenshots.json` records the version they show and a hash of the sources that draw them; CI warns while
those sources differ from the ones the images were rendered from, and a stable release refuses stale images. See
[DEVELOPMENT](../DEVELOPMENT.md) for the details.
