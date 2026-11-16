#!/usr/bin/env python3
"""Puts a screenshot the app drew onto the screen of the README's device photo.

Usage: compose.py <device.png> <screen.png> <out.png>

The display area of brick-hammer-hero.png was measured once (its first and last dark rows
and columns); the screenshot is scaled to fill it, as the Muse hero's screen does.
"""
import sys
from PIL import Image

SCREEN = (129, 41, 958, 738)  # left, top, right, bottom (exclusive)

device, screen, out = sys.argv[1:4]
photo = Image.open(device).convert("RGBA")
shot = Image.open(screen).convert("RGBA")
size = (SCREEN[2] - SCREEN[0], SCREEN[3] - SCREEN[1])
photo.paste(shot.resize(size, Image.LANCZOS), SCREEN[:2])
photo.save(out, optimize=True)
