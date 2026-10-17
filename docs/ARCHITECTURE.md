# Architecture

## Layers
1. **core** (pure Rust, no OS): `cdb` builders, `pages` (feature 0xFF33, mode pages 0x31/0x32),
   `media_id` (Reed-Solomon decode and field layout), `media_profile` (per-disc pitch and tone
   curve), `geometry` (track <-> radius, dots per track, calibrated angle origin), `track` (header
   and bit packing), `raster` (polar sampling and error diffusion), `plan` (ordered command steps).
2. **transport** (per OS): executes one SCSI command (CDB + optional data in/out) and returns
   status and sense. The same safety guard (`guard::Guarded`) sits in front of every backend:
   opcode allowlist, and the print and MODE SELECT opcodes refused until the burn is armed.
3. **executor**: runs a `plan::Step` list against a transport. It logs every command, stops on
   the first unexpected CHECK CONDITION, treats 02/04/08 as "busy, wait and retry", and on every
   exit path closes an unfinished job with the last-track marker, sends STOP, restores and
   verifies the labeling page if the pitch was changed, and sends ALLOW MEDIUM REMOVAL.
4. **front ends**: CLI first, UI later.

## Transports
| OS | Mechanism | Notes |
|---|---|---|
| macOS | IOKit SCSITask user client with exclusive access, via a small C shim built by `cc` (rather than re-declaring the COM-style vtables in Rust) | opening fails if a volume is mounted; label-side-down LightScribe discs have none. Same calls as the C prototypes (`lsprobe`, `lsburn`, not in this repository) |
| Linux | `ioctl(SG_IO)` on `/dev/sr0` | needs access to the device (group `cdrom`); unmount first |
| Windows | `DeviceIoControl(IOCTL_SCSI_PASS_THROUGH_DIRECT)` on `\\.\X:` | needs administrator rights for the vendor commands; untested |

Only the macOS path has real hardware behind it. Linux and Windows backends must be tested on
those systems before they are trusted.

## Calibrated constants (HP 1040r / Memorex, Draft-to-Best pitch)
- Dot 0 at -17 degrees (clockwise from image up, label-side view), same at every radius.
- Dots run clockwise from the label side; header track shift adds in that direction (0.9 degrees per spoke).
- Radius = 23 800 um + track * pitch; dots per track = floor(2*pi*r / mark pitch) + 1.
- One print command per track (16-byte header + bit-packed data, 1 = mark); the drive burns one
  revolution per track.
- A coarse light line about every 0.3 mm remains at every pitch (fixed radii, tentative; not caused by our data, feed order, START or burn start). Treated as a drive or disc trait; a second-disc burn would tell which.
- Pitch change is the only MODE SELECT the tool makes (page 0x31 bytes 14-16), restored afterwards.
  Pitches finer than 25 400 nm (the finest burned) are refused, automatic or by hand.

## Drive behaviour (1040r, seen on the bench)
- The drive starts writing on its own after ~11 tracks (no START needed), takes tracks in batches
  of 6, and answers 02/04/08 while writing.
- TEST UNIT READY can be GOOD while STOP is still answered 02/04/08, so cleanup retries the
  marker and STOP themselves until they are accepted.
- An unfinished job (no last-track marker) leaves the tray locked, so cleanup always sends the marker.

## Media and drives
- Per-disc profile: `media_profile::profile()` gives the pitch and tone curve for a disc.
  Measured: Memorex 104 (25.4 um, convex curve), Verbatim 122 (26.7 um, nearly the Memorex shape)
  and HP 116 (31.4 um, concave curve). Other media: pitch scaled from HP's table (Memorex pitch
  times Memorex Best tpi / this media's Best tpi, 19 media types in a data file), with a fallback
  from the disc's maximum power at 250 mm/s for unlisted media; Memorex curve as an estimate.
  `--pitch auto --tone auto` reports which. HP 116 was measured at HP's own Best pitch, finer
  than the scaled 38.1 um.
- Other drives: drive-specific numbers come from HP's table (123 drives): write-power conversion
  and per-media track resolutions. A measured pitch is carried to another drive by the ratio of
  the scaled pitch on that drive to the scaled pitch on the 1040r. Tone curves and absolute
  pitches were measured on the 1040r only, so on other drives they are flagged as estimates. The
  labeling page layouts accepted are the 56-byte (v4+) and 44-byte forms; the page codes come
  from feature 0xFF33; the print opcode must be 0xFC.

## Not done yet
- A second, independent tone measurement for Memorex (its curve comes from one photo); a Draft/Normal curve.
- Checking that HP's Best pitch equalises contrast on brands other than the three measured.
- Any drive other than the 1040r; Linux and Windows on real hardware.
- Label shape 2 (23.8-37.5 mm) media.
