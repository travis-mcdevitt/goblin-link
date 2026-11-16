# README hero images

`brick-hammer-hero.png` is an illustrative, AI-assisted screen composite, not a photo
of Goblin Link running on hardware. It appears immediately below the main README title.

- Device reference: the grey variant on the [TRIMUI Store Brick Hammer product page](https://www.trimuistore.com/products/trimui-brick-hammer-handheld-game-console?variant=46759754039552).
- Original 1500 × 1500 front-view image: [grey Brick Hammer photo](https://www.trimuistore.com/cdn/shop/files/BRICK_HAMMER_EN_01_667f6591-b1d9-44bd-9ef2-0a522591deb6.jpg?v=1761893782), retrieved 2026-11-14. Product photography belongs to its respective rights holder; no project-license grant is claimed for it.
- Screen reference: [`store/1-chat.jpg`](store/1-chat.jpg), the existing dark-mode “Ask it about your games” screenshot.
- Composition: built-in image generation/editing tool, using both images as references. The screen reference was not overwritten.
- The PNG retains transparent alpha. The README retains its reduced display width of 283 pixels.

Initial prompt:

> Use case: compositing. Create the replacement Goblin Link README hero. Image 1 is the edit target: the exact GREY TrimUI Brick Hammer product photo. Image 2 is the supporting screen insert: the exact dark-mode Goblin Link RPG chat screenshot. Preserve the grey metal device's geometry, grey finish, black buttons, speaker holes, side buttons, BRICK HAMMER label and straight-on perspective exactly. Replace only the screen contents with the COMPLETE image 2, centered within the display, preserving its 4:3 aspect ratio and all text, avatar, blue user bubble, dark reply bubble, suggestion pills and composer. Do not add a heading or rewrite any screen text. Remove the white background outside the device, with genuine transparent alpha (no checkerboard pixels, halo or drop shadow). Tight portrait crop with a small transparent margin around the entire device. High-resolution crisp photoreal product composite. No additional objects, captions, invented controls or UI. Keep the screen DARK as supplied.

Final correction prompt:

> Correct this composite. Image 1 is the edit target. Image 2 is the EXACT dark Goblin Link screenshot. CRITICAL: the TRIMUI logo and 23:34 status row across the top of image 1 are OLD SCREEN CONTENT, not physical device markings. REMOVE that entire old TRIMUI header. Fill the ENTIRE black rectangular display, from its top inner silver edge down to the physical BRICK HAMMER label, with image 2 edge to edge. There must be ONLY ONE status row, the Goblin screenshot's 7:58 row, and no TRIMUI logo anywhere. Do not inset the screenshot into a smaller box; it occupies the whole display. Preserve complete screenshot, all text verbatim and dark theme. Keep grey device body, buttons, geometry, label below screen, transparency and crop unchanged. Genuine transparent background.

## Hermes hero

`brick-hammer-hermes-hero.png` is the same device photo with a screen the app drew itself, not
an image model: Goblin Link's own dark chat on a Hermes profile, in its gold, with a made-up
exchange about a Home Assistant house. To make it again:

```sh
cargo build -p goblin-ui
target/debug/goblin-ui --size 1024x768 --dark --ui-size comfortable \
    --screenshot chat@hero-hermes hermes-chat.png
python3 tools/hero/compose.py docs/images/brick-hammer-hero.png hermes-chat.png \
    docs/images/brick-hammer-hermes-hero.png
```

`compose.py` scales the screenshot to fill the photo's display area, measured once, as the
Muse hero's screen fills it.
