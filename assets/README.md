# assets

## earth_mask.bin

A packed land/ocean bitmap used by the `lattice` TUI welcome scene to draw the
Earth with real coastlines.

- **Dimensions:** 360 × 180 cells (1° per cell), equirectangular. Row 0 is
  latitude +90° (north), column 0 is longitude −180°.
- **Format:** 1 bit per cell, row-major, MSB first; a set bit means land.
  360 × 180 = 64 800 bits = 8100 bytes.
- **Provenance:** derived by downsampling NASA's Blue Marble "Land shallow topo"
  equirectangular image (public domain) and classifying each cell as ocean
  (dark, blue-dominant) or land. NASA imagery is in the public domain; this
  derived bitmap carries no additional restrictions.

The bitmap is stored as a fixed asset. Its source is NASA's Blue Marble
"Land shallow topo" equirectangular image. Downsampling and threshold choices
can change coastline cells; the description above explains the provenance,
not a byte-for-byte regeneration recipe. Keep this attribution with the asset.
