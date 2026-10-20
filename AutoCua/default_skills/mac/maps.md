This is additional domain knowledge for the Apple Maps app on macOS (use it wisely).
<additional_knowledge>
1. Input fields do not auto-clear — clear the field first (click the 'x' on the field) before typing a new value.
2. Directions with multiple stops:
    2.1. Multiple stops require travel mode 'Car' (walking does not support stops). Search the start, open Directions, choose Car, then "Add Stop" and type each location.
    2.2. Reorder stops by drag and drop.
3. Routes (build a custom route across a small/close area, connecting one place to the next):
    3.1. Find the place first, then switch to Routes.
    3.2. Place each point by clicking its OCR_TEXT on the map. On a busy canvas with many labels, click the correct OCR_TEXT; if you pick the wrong one, click the Undo button to remove it.
4. Fastest way to get directions — AppleScript (Maps URL scheme). This needs almost no UI info: just start, destination, and mode, so prefer it over GUI clicks for plain point-to-point directions.
    4.1. URL: https://maps.apple.com/?saddr=<start>&daddr=<destination>&dirflg=<mode>  — dirflg is d (drive), w (walk), or r (transit). URL-encode spaces as %20.
    4.2. Eg: applescript {"app": "Maps", "value": "open location \"https://maps.apple.com/?saddr=London%20Bridge,%20London&daddr=London%20Eye,%20London&dirflg=w\""}
    4.3. To show several legs in turn, chain calls in one script: open location "…A→B…" then `delay 4` then open location "…B→C…".
5. Zoom in and out using the buttons on the right side.
</additional_knowledge>
