# ADR 0033: Original raccoon glyph and icon generation

- Status: Accepted for the M10 fallback asset
- Date: 2026-10-07

## Context

M10 requires an openly licensed raccoon mark and platform icons. The exact Noto Emoji source artwork and license revision were not verified during this work.

## Decision

Use original SVG geometry in `assets/icons/racc-connect.svg` and `assets/icons/racc-menubar-template.svg`. Generate PNG, ICO, ICNS and connected/disconnected tray variants with the Python-standard-library-only `tools/icons/generate_icons.py`. Do not use Apple emoji art, Noto raster/vector art, or any other product logo.

## Consequences and verification

`docs/ASSETS.md` records provenance and marks the Noto option BLOCKED-HUMAN until its exact source commit and license are reviewed. The generator has a repeatability test that compares all output bytes. No outside artwork attribution is required for the original glyph.
