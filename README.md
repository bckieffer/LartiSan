# LartiSan

Open, cross-platform LightScribe labeling. Rust workspace:

| Crate | What it is | Status |
|---|---|---|
| `lartisan-core` | Portable protocol, mode pages, media ID decode, geometry, polar rasterizer, per-media profiles, burn planner. No OS code. | working, golden-tested against real captures and burns |
| `lartisan` (CLI) | `plan` (offline dry run), `probe` (read-only), `burn` (live, only with `--arm` and a typed confirmation); patterns `ring`, `f`, `wedge`, `band`, `image` (PNG) | working on macOS |
| `lartisan-scsi` | `ScsiTransport` trait, safety guard, drive simulator, executor, backends: macOS (IOKit via C shim), Linux (SG_IO), Windows (pass-through) | macOS used on real hardware; Linux and Windows compile-checked only, untested |
| app | label designer UI | not started (framework undecided) |

```
cargo test                                          # golden and simulator tests
cargo run -p lartisan -- probe                      # read-only: drive, disc and decoded media ID
cargo run -p lartisan -- burn image --image art.png --size-mm 110 --pitch auto --tone auto
                                                    # preflight and plan; add --arm to burn
cargo run -p lartisan -- plan f                     # offline dry run of the F test pattern
cargo run -p lartisan -- plan image --image art.png --size-mm 110 --pitch auto --tone auto \
    --media-id "5D 1E 9B 74 1C 19 65 27 26 15 0F 0F 8D 54 9C"   # plan for a disc that is not loaded
```

Images can be placed with `--offset X Y` (millimetres from the disc centre) and `--rotate DEG`.
Run `lartisan` with no arguments for every option.

## Hardware and media

**Drives:** any drive that reports LightScribe (feature 0xFF33) with the print opcode 0xFC is
accepted. Its page codes come from that feature, its write power is converted with HP's per-drive table (123 drives), and its
labeling page must have one of the two known layouts; anything else is refused rather than guessed
at. Only the HP DVD Writer 1040r (DriveID 169) has actually been used. The pitches and tone
curves below were measured on it, so on any other drive they are estimates, and the CLI says so:
burn a tone wedge first.

**Discs:** the disc's media ID is read and decoded (Reed-Solomon RS(15,11) over GF(128)), then:

| Disc | Pitch | Tone curve |
|---|---|---|
| Memorex LightScribe CD-R (imaging parameter 104) | 25.4 µm | measured |
| Verbatim LightScribe DVD-R (122) | 26.7 µm | measured |
| HP LightScribe DVD-R (116) | 31.4 µm | measured (accuracy about ±0.08) |
| Any other disc | scaled from HP's per-media table (19 media types), or estimated from the disc's reported power | Memorex shape, flagged as an estimate on screen |

Any disc is accepted when its ID decodes and the drive's write power (28.7 mW on the 1040r) lies
inside the disc's own power window at the drive's speed. A disc that fails either check is refused, and
only the full label shape (23.8 to 58.7 mm) is supported. The write-power check uses the drive's own
conversion and its current labeling speed, so it holds on any drive in HP's table. Darkness on a disc with an estimated
curve may differ from the calibrated ones; a tone wedge burned on that disc would measure it.

Track pitch is never set finer than 25.4 µm (the finest burned so far), whether chosen with
`--pitch auto` or by hand with `--pitch-nm`.

See `docs/ARCHITECTURE.md` for the design, the protocol facts and the open items.
