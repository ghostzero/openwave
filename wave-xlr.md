# Elgato Wave XLR — reverse-engineering notes

## Software volume curve (PipeWire/pactl)

`pactl set-source-volume` / `set-sink-volume` percentages are **not** linear in
signal amplitude. Measured directly from `pactl get-source-volume` /
`get-sink-volume` output on this system:

| percent | dB      |
|--------:|--------:|
|      1% | -120.01 |
|      5% |  -78.07 |
|      8% |  -65.82 |
|      7% |  -69.30 |
|     20% |  -41.94 |
|     30% |  -31.37 |
|     34% |  -28.11 |
|     50% |  -18.06 |
|     51% |  -17.55 |
|     70% |   -9.29 |
|     92% |   -2.17 |

This matches a cubic law exactly:

```
amplitude = (percent / 100) ^ 3
dB        = 20 * log10(amplitude)
```

e.g. 70% → 0.7³ = 0.343 → 20·log10(0.343) = -9.29 dB. Confirmed to match
`pactl`'s reported dB at every measured point above. This is a deliberate
perceptual/loudness-style taper, not a hardware property — it's PipeWire's
software volume scaling, applied before the gain value reaches the device.

## Physical LED ring (hardware meter)

The Wave XLR's onboard LED ring (25 segments) reflects the incoming
gain/dB level, but through its own non-linear meter curve — separate from
and not identical to the OS's cubic percent curve above. It was **not**
possible to find this mapping in the OpenWave driver source; the ring
appears to be driven by the device's own firmware directly from the
gain value, not by any userspace meter logic.

**The mic input (source) and output (sink) do NOT share the same LED
curve.** Initially assumed they did (single cross-check point at 100%
matched on both), but a second cross-check at 30% falsified that: 30%
gives 15 LEDs on the mic input but only 12 LEDs on the output. The two
controls must drive the ring through separate firmware curves that
happen to coincide only at the very top (0 dB / 25 LEDs).

### Mic input (source)

Data points gathered empirically (`pactl set-source-volume` on the mono
mic input, LED count read visually off the physical device). Full sweep
from fully dark to fully lit:

| percent | dB (software) | LEDs lit (of 25) |
|--------:|---------------:|------------------:|
|      5% |         -78.07 |                  0 (ring fully dark) |
|      6% |         -73.31 |                  1 |
|      7% |         -69.30 |                  2 |
|      8% |         -65.82 |                  3 |
|     20% |         -41.94 |                 11 |
|     30% |         -31.37 |                 15 |
|     34% |         -28.11 |                 16 |
|     45% |         -20.81 |                 18 |
|     51% |         -17.55 |                 19 |
|     70% |          -9.29 |                 21 |
|     92% |          -2.17 |                 24 |

A linear least-squares fit of `LED ≈ a·dB + b` over these 11 points gives:

```
LED (source) ≈ 0.321 * dB + 24.6
```

(≈ 3.11 dB per LED segment, holding fairly constant from 0 to 24 LEDs).

### Output (sink)

Full sweep from fully dark to fully lit, gathered the same way
(`pactl set-sink-volume` on the analog-stereo output):

| percent | dB (software) | LEDs lit (of 25) |
|--------:|---------------:|------------------:|
|     10% |         -60.00 |                  0 (ring fully dark) |
|     11% |         -57.52 |                  1 |
|     12% |         -55.25 |                  2 |
|     21% |         -40.67 |                  8 |
|     30% |         -31.37 |                 12 |
|     40% |         -23.88 |                 15 |
|     58% |         -14.19 |                 19 |
|     76% |          -7.15 |                 22 |
|    100% |           0.00 |                 25 (ring fully lit) |

A linear least-squares fit of `LED ≈ a·dB + b` over these 9 points gives:

```
LED (sink) ≈ 0.416 * dB + 25.0
```

(≈ 2.40 dB per LED segment). This fit turned out to be extremely tight —
every predicted percentage from the running fit landed on the exact
target LED count when tested live, across the full range from 0 to 25.
So unlike the source curve (a reasonable but imperfect linear
approximation), the sink's LED response looks like a genuinely linear
function of dB.

**Source vs. sink compared:** both curves converge only at the very top
(0 dB → 25 LEDs), but diverge increasingly at lower levels — the sink
slope (0.416 LED/dB) is noticeably steeper than the source slope (0.321
LED/dB). Concretely, at -31.37 dB, the source shows 15 LEDs but the sink
shows only 12: the same dB value maps to different LED counts depending
on which control (source gain vs. sink volume) produced it. This
confirms the two controls drive the ring through separate firmware
curves, not a shared one.

### Reverse the fits (percent → target LED count)

Mic input (source):
```
dB      = (L - 24.6) / 0.321
amplitude = 10 ^ (dB / 20)
percent = 100 * amplitude ^ (1/3)
```

Output (sink):
```
dB      = (L - 25.0) / 0.416
amplitude = 10 ^ (dB / 20)
percent = 100 * amplitude ^ (1/3)
```

### Open questions

- Exact firmware curve is unconfirmed for both source and sink — these
  are empirical linear fits from live back-and-forth calibration against
  the physical LED ring, not derived from firmware/protocol docs.
- The source fit is a reasonable but visibly imperfect line (small
  residuals at a few points); the sink fit has matched every prediction
  exactly so far across 9 points spanning the full range. Unclear why the
  two controls would differ in how cleanly linear they are — possibly the
  source curve has more real firmware curvature, or just more measurement
  noise from reading LEDs on a smaller/less distinct portion of the ring.

## Physical gain wheel is not readable from the OS

The device has a physical gain wheel that moves the LED ring directly
(independent of any `pactl`/PipeWire command). Tested whether turning it
by hand shows up anywhere in the OS:

- `pactl get-source-volume` on the mono mic input stayed frozen at
  whatever value was last set in software (45%) across two different
  physical wheel positions.
- ALSA exposes a `Mic Capture Volume` control on the card
  (`amixer -c <card> contents`, numid=6, raw range 0-150, hardware dB
  range 0.00-75.00 dB — note this is a completely different scale/domain
  than the `-x dB` pactl reports, which is PipeWire's own software cubic
  curve). Its raw value stayed at exactly 108 across three separate
  checks spanning a turn-down, a turn-up, and a third arbitrary wheel
  position — it never changed once.

Conclusion: the wheel's effect on the LED ring is local to the device's
own firmware and does not propagate back to the host. The `Mic Capture
Volume` ALSA control lacks the "volatile" access flag (its access string
is `rw---R--`, no `V`), which is what would make the ALSA driver re-poll
hardware state on every read — without it, the control just echoes back
whatever the host last wrote, not the device's actual live gain. So
**there is currently no way to read the wheel's physical position from
the OS** through the standard ALSA/PipeWire control surface; software
volume changes (`pactl`) can drive the LED ring, but the reverse
direction (hardware → OS) doesn't work for this control.
