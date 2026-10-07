# EyeTracking

A native Rust desktop companion for the Tobii Eye Tracker 5 on Windows.
Look at a target, then move your mouse or slide on your trackpad to land there.
The control panel brings gaze control, scrolling, calibration, and learning together.

## Run

```powershell
Set-Location C:\Users\andre\RustroverProjects\EyeTracking
cargo run --release
```

Or run `target\release\gaze-preview.exe` after building. Gaze control starts enabled
on the first launch; subsequent launches remember your switches. The app uses the
installed DLL at `C:\Program Files\Tobii\Tobii EyeX\tobii_stream_engine.dll`.
An absolute `TOBII_STREAM_ENGINE_DLL` environment variable can override that path.
No SDK download or import library is required. The adapter currently accepts
Stream Engine major version 4; other major versions need an ABI review.

## Control panel and system tray

- **Pause / Resume gaze control** controls mouse and trackpad assistance.
- **Gaze dot**, **Gaze scroll**, and **Learning** have independent visible switches.
  **Learning visuals** independently controls brief, click-through feedback near
  the correction. Orange marks the original landing; white marks the click;
  mint animates the actual change in predicted landing and reports its pixel size.
  Accepted clicks without consensus say that no map change happened. Rejected
  long corrections/drags/held or ambiguous clicks explain why they were skipped;
  a white selection is shown only when a click-down position was captured.
  The animation fades within 2.4 seconds (1.5 for rejections) and never moves the
  real cursor or captures input. Turning visuals off hides an active animation
  immediately and keeps learning running. The visual preference is saved.
  **Preview animation** shows a clearly labeled simulation without training the
  learner. The Overview also shows the latest learning status beside its counters.
  Freezing learning keeps the current map; **Reset learning** clears the session's
  learned corrections after confirmation, keeping saved journals and calibration.
- **Jump timing** has separate mouse and trackpad fields. Mouse accepts 50–2000 ms
  (default **300**) of idle time before the next movement can jump. Trackpad accepts
  0–2000 ms (default **0**) between landing attempts, while still requiring a lift
  and a new slide. A contact started during that delay stays in fine control until
  lifted; it never produces a late jump. Zero preserves immediate rearming on lift.
  Select **Apply** or press Enter to save both values. Scrolling and Windows'
  double-click protection interval are unchanged.
- **Calibrate** opens the guided dots. Space is convenient for capturing without
  moving the pointer; **Capture dot** and **Cancel calibration** are also buttons.
- **Live preview**, **Calibration results**, and **View learning map** expose the
  live gaze, measured accuracy, and saved click evidence.
- **Close (×)** or **Hide to tray** hides only the panel. Tracking and the desktop
  dot keep running. Click the mint eye icon beside the clock (possibly inside the
  hidden-icons menu) to reopen. Right-click it for pause, dot, and quit controls.
- **Quit EyeTracking** stops tracking and exits. Launching the app again while it
  is already running brings back the existing panel instead of starting another.

Controls support Tab and Space/Enter. No global shortcut is required. The old
shortcuts below remain optional; a shortcut conflict no longer prevents input
from starting. Tracker connection, paused state, display changes, and learning
counts are visible in the panel. The tray icon is restored if Explorer restarts;
if the tray is unavailable, closing keeps the panel accessible instead of hiding it.

Switches save in `recordings/preferences.json`. Development builds locate the
repository automatically, including launches from Explorer or a shortcut. A
standalone executable uses `%LOCALAPPDATA%\EyeTracking` for its data. Journals and
calibration remain private local files; the live learned field is still session-local.

In **Live preview**, the mint ring shows the latest valid gaze position, with a short fading trail.
The small stationary dots are visual references, not a calibration procedure.
No additional smoothing or prediction is applied by this app; the vendor stream
may already be processed. Invalid/stale samples hide the marker. A worker thread
handles device access and retries connection failures.

| Key | Action |
| --- | --- |
| C | Start a full-screen calibration experiment |
| R | Toggle results map / live preview after calibration |
| A | Toggle this app's correction on/off after calibration |
| Ctrl+Alt+F7 | Show/hide the desktop gaze dot globally |
| Ctrl+Alt+F6 | Show/hide the status panel globally (hidden on startup) |
| W / Ctrl+Alt+F8 | Toggle gaze mouse (Ctrl+Alt+F8 works globally) |
| Ctrl+Alt+F9 | Freeze/resume continuous learning; keep the current correction map |
| Ctrl+Alt+F10 | Reset the learned correction map and click history |
| L | Open a snapshot of saved continuous learning in your browser |
| F11 | Toggle full screen |
| M | Move preview to the next display |
| T | Toggle gaze trail |
| G | Toggle fixed targets |
| Space | Capture the active calibration dot; otherwise hide/show gaze |
| Esc | Return to Overview, cancelling an active calibration |

## Calibration experiment

Select the tracker’s configured screen using M, then press C. Look at the bright
dot and press Space. Keep looking until it moves: each capture allows 0.8 seconds
to settle and collects for 1.6 seconds. Press Space separately at each new dot.
The gaze marker is hidden throughout calibration to avoid chasing feedback.
Losing window focus pauses the capture; changing display geometry cancels the run.

The first 25 dots fit a correction to the existing gaze output. A 5 × 5 layout
reaches 4% from each screen edge, with an additional band at 18%/82% so the
upper corners and other edges have nearby support. Targets alternate across the
screen rather than sweeping one row at a time. Instructions and capture buttons
move to the opposite half of the screen so they cannot cover the active target.
Live feedback shows whether gaze is currently detected. Capture in your normal
sitting position: an area that loses eye tracking cannot be fixed by a coordinate
map. Insufficient valid data keeps the same target for retry and never trains on
the missing gaze. Cancel preserves the saved calibration if positioning cannot
provide usable coverage; ordinary mouse/trackpad movement remains the fallback.

Per-axis median
absolute deviation (MAD) rejects samples with modified Z scores over 3.5, with a
2-pixel MAD floor for quantization. The remaining samples are averaged. Rejection
does not depend on closeness to the target. Captures need at least 25 retained
samples, 65% valid samples, and 70% retained among valid samples. Excess scatter
(2.5% of display diagonal) or drift (2% of diagonal, first vs last third) requests
a retry. These thresholds are experimental, not a certified fixation detector.

An affine residual map corrects offset, scale, and skew. A regularized quadratic
map replaces it if leave-one-target-out training RMS improves by at least 10%
and more than 1 pixel. A local map adds smooth Gaussian residuals to an affine
baseline, so different regions can correct in different directions. Its width
and regularization are chosen from a fixed grid using only leave-one-target-out
training RMS; both the baseline and local field are refitted inside each fold.
The local map is selected when its training prediction error beats the global
map (ties within 0.001 pixel keep the global map). Each target has equal weight.
Models with folding, extreme stretching, or excessive correction are rejected
on a 21-by-21 grid. Corrections use bounded features outside the screen, while
the gaze position itself remains unclamped.
The map is frozen before collecting 12 new validation dots: four near the corners,
four near edge midpoints, and four in the interior, all separate from training.

Results show original and corrected mean-target error, the worst target, and RMS
error across ALL valid validation samples (including centroid outliers), weighted
equally by target. The results page has side-by-side, screen-shaped before/after
maps that remain fully visible in a normal window. White rings are check targets;
dots and lines show measured means and their distance from the target. Each target
is labeled with its error in full-display pixels. Mint means improved; red means
worse. Off-screen means are drawn at the edge, but error values use their actual
coordinates. A prominent status says whether the correction is applied or only
being previewed.

Correction turns on automatically only when validation mean-target error improves
by over 10% and 2 pixels, all-valid-sample RMS improves, at least 9 of 12 targets
improve, and the worst target does not regress by more than 10%. Every checked
corner must also avoid regression beyond 10% or 2 pixels (whichever is larger).
Results list each corner's before/after error; older reports explicitly show
that their corners were not checked. A toggles the map
for comparison, including an experimental override when the checks do not pass.
This is a short within-session check, not evidence of long-term calibration quality.

Completed runs save observations, rejected points, means, model coefficients,
display geometry, and validation metrics in `recordings/calibration-*.json`, excluded
from Git. The correction is local to this app and only applies on the same display
name/geometry. The latest valid saved report is restored on startup; redo calibration
if posture or Tobii calibration changes. Tobii's calibration is never overwritten.

**Refit saved points** reuses the saved fitting captures, compares the new model
on the saved check captures, and saves a new report before applying it. The original
recording is kept. No fresh gaze is collected, and the UI explicitly labels this
as a replay of saved checks. Replayed results are retrospective evidence, not a
new independent accuracy test. The same automatic-enable rules still apply.
Reports record the source timestamp and previous metrics. Version 2 stores the
local field and remains readable across restarts; version 1 recordings remain
loadable. Older binaries skip version 2 rather than misapplying only its baseline.

To compare recordings without modifying them or connecting to the tracker:

```powershell
cargo run --example calibration_replay -- recordings/calibration-<timestamp>.json
```

With no path arguments, the replay tool verifies loading the latest saved map
through the same loader used at app startup.

Method references:
- https://www.itl.nist.gov/div898/handbook/eda/section3/eda35h.htm
- https://connect.tobii.com/s/article/eye-tracker-calibration?language=en_US

Use the display configured in Tobii Experience. The preview assumes the monitor
containing its window is that display. M changes only this app's mapping/location;
it does not reconfigure Tobii. Gaze maps to the full physical monitor and then
to the window's client coordinates, accounting for DPI and monitor origin.
Gaze outside the window is not clamped to its edges. Full screen makes the entire
display available for preview. Mouse control is an optional mode described below.

## Gaze mouse

Use **Resume gaze control** in the panel. `--mouse` still forces an enabled start
for existing launch scripts, overriding a saved paused preference.
Turn off Tobii Experience's own **Warp on mouse move** to avoid two controllers.
On a supported touchpad, single-finger contact arms the first raw pointer movement
to jump to gaze. Touching, tapping, and double tapping without movement never
jump. Start a small slide to land, then continue sliding to refine and click.
No extra distance threshold is added beyond the driver's first pointer delta.
Lift all fingers to rearm; resting your finger never rearms, regardless of how
long you pause. Clicks protect the pointer for the Windows double-click interval
so incidental movement cannot interrupt the second click. A
contact that starts while paused, dragging, holding a modifier, or using multiple
fingers cannot cause a delayed jump when that condition ends. Two-finger scrolling
keeps its existing gaze-focus behavior. If the second finger arrives after the
first and you slide before it arrives, that slide may land before scrolling starts.

Until a supported contact report is received, the old first-motion fallback uses
a 300 ms idle interval. Once contact mode is detected, Windows' null-device
touchpad pointer packets can land only once per eligible single-finger contact.
Physical mice with their own device handles always keep independent 300 ms
movement bursts, even while a touchpad is connected. The dot and HUD follow the
last-used pointer source; starting a fresh touch selects touchpad feedback.
Disconnecting all recognized touchpads restores motion fallback for null-device
input as well. Double-click protection applies to both mouse and touchpad.

`cargo run --example touch_probe` records 60 seconds of passive contact/mouse
timing and cursor position/visibility changes to `recordings/touch-probe-*.jsonl`.
It does not inject input. Use it to distinguish contact delivery from visible
pointer behavior; compare its first contact with the normal learning journal's
`jump` events (`source: slide` versus `mouse`). These local traces are ignored
by Git. Earlier `source: touch` records belong to the retired contact-only mode.

A click-through status panel shows ARMED, FINE CONTROL, PAUSED or NO GAZE, plus
learning and recording status. It starts hidden; **Ctrl+Alt+F6** shows or hides it
without changing gaze control, the dot, or learning. The panel also includes
successful and missed jump counts. **Ctrl+Alt+F8** pauses/resumes from any app;
closing the panel keeps the controller running in the system tray. **Quit** stops
it. It runs on a separate input thread.
A tiny gaze dot with a one-pixel white rim stays above desktop apps,
including while the preview is minimized or mouse jumps are paused. Its center
uses the same calibrated position as a jump. The dot is nine physical pixels
wide, takes no focus and passes clicks through. It hides on tracking loss or
during calibration, so it never shows an old position as live gaze.
Black means the next slide is armed to land. Orange means fine control after a
slide has landed, held buttons or modifiers, double-click protection, the
fallback's 300 ms rearm interval, and paused
mouse assistance. Color refreshes every 16 ms, even if the gaze position is still.
**Ctrl+Alt+F7** shows/hides just the desktop dot from any app; mouse jumps,
gaze scrolling, and learning continue unchanged. The HUD shows Dot ON/OFF.
The dot switch is remembered between launches.
Raw Input accepts precision touchpads with null device handles, and cursor warps
do not feed back as physical motion. The latest gaze sample must be valid and no
older than 200 ms. A missing sample consumes that landing attempt with a visible
reason; it cannot create an unexpected delayed jump when tracking recovers.

Mouse mapping stays on the saved calibration's display, independently of the
preview window. Validated correction is applied automatically. A display layout
change suspends warps until the saved layout is restored or calibration is redone.
Without a saved report it uses raw gaze on the preview's initial display.

### Continuous calibration

Learning is on by default while gaze mouse is enabled. A successful jump saves
the gaze estimate before adaptation. A short correction followed by a left click
provides a tentative target: click position minus that saved base estimate.
The click is committed on release so dragging does not train the model. No gaze
samples taken after the jump are used as labels. Successful clicks requiring no
correction count too, so learning can settle instead of continually overshooting.

Eligible clicks start 80–1500 ms after the jump and release within 500 ms; the
correction is at most 120 physical pixels, with at most 240 pixels total travel.
Scrolling, other buttons, keyboard modifiers, crossing top-level windows, edge
clamping, dragging over 4 pixels, and delayed input invalidate the attempt. These
are learning filters only; slide-to-land adds no travel threshold.

The learner maintains a 7-column by 5-row field of local XY corrections over the
base gaze position. Both horizontal and vertical errors can vary with both screen
coordinates. Bilinear interpolation makes the correction continuous between grid
points; areas without nearby evidence retain the base calibration.

Up to 256 candidate labels are kept for 5 minutes. Each grid point considers its
last 9 nearby labels, within a radius of 0.30 in normalized screen coordinates.
Weights taper smoothly to zero at that radius and decay with a 1-minute half-life.
At least 5 labels must agree within 20 pixels of their weighted median and make up
60% of the local weight. A disagreeing new click cannot update that grid point.
Updates move 15% toward local consensus, scaled by the new click's proximity,
limited to 2 pixels per click and 80 pixels total. Neighboring corrections also
have a spatial gradient limit to prevent folding the map. The dot, live preview,
jumps and gaze scrolling all use the same position-dependent correction.

The HUD shows accepted clicks, applied updates, trained grid points out of 35,
and the most recent learning status. Ctrl+Alt+F9 freezes learning without removing
the correction map; Ctrl+Alt+F10 resets
it. Starting calibration or toggling its base correction resets adaptation too.
Correction learns for the current run: restart starts fresh rather than applying
a previous sitting position. Learning evidence is saved across runs (see below).
It does not rewrite the saved model or record browsing contents.
Click targets are heuristics, so improved real-world
accuracy still needs to be evaluated during use.

### Learning history and map

Learning is evaluated on each eligible left-button release, rather than at a
fixed interval or on each gaze sample. Five consistent nearby labels are required
before a region can change. The HUD counts eligible clicks separately from clicks
that actually changed the correction field.

Every resolved learning attempt is appended to `recordings/learning-*.jsonl`.
Each record includes its timestamp, physical display rectangle, base gaze estimate
(after fixed calibration, before online correction), actual pointer landing,
click target when available, last cursor position, travel, eligibility, update
outcome, reason, and the resulting 7×5 field. Context records include the display
and fixed calibration model. Resets begin new periods without deleting history.
Only attempts initiated by an eligible gaze jump are recorded, not the full gaze
stream or every desktop click. Saving and report generation run on a separate
writer thread; the HUD reports save errors. Events are flushed after each record.
The data stays local and recordings are excluded from Git.

Press **L in the preview** to generate and open the current session's standalone
HTML map. It shows base gaze → landing → click, the learned field, outcome filters,
separate calibration periods, magnified arrows, and clickable point/row details.
It is a snapshot: press L again for an updated snapshot. A matching HTML snapshot
is also saved at normal exit. Use **Open saved JSONL…** to inspect another run.
Both files remain under `recordings/`; the online learner does not automatically
reload old corrections. Data from runs before this feature cannot be recovered.

### Gaze-directed scrolling

With gaze mouse enabled, the first vertical or horizontal wheel event after a
500 ms scroll pause moves the pointer to the gaze dot and attempts to activate
that window. The original first event is replayed once with its signed delta
intact. During scrolling, looking at another window for 120 ms moves the pointer
and focus there; movement within the same window does not chase your eyes.
Mouse wheels use the wheel hook. The Apple Magic Trackpad running the GitHub
MagicTrackpad2ForWindows driver also exposes Precision Touchpad digitizer reports:
the app reads those passively using the descriptor parser from TrackpadGlass.
Two confident fingers moving together begin gaze targeting; stable gaze changes
can switch windows while those fingers remain down. This does not consume or
synthesize Windows' native touchpad scrolling, so apps that capture a native
gesture may still require lift-off before they deliver scroll to the new window.
Pinches, rotations, three-finger gestures and pressed clicks are excluded.
The touchpad HUD counts parsed reports and recognized scroll gestures. A small
parallel movement (0.15% of pad extent for both fingers) distinguishes scrolling
from resting fingers; it does not change the mouse-jump threshold.
Ctrl/Alt/Shift scrolling and gestures while holding a mouse
button retain their normal behavior. Scrolling never becomes a calibration label.

Ctrl+Alt+F8 pauses gaze scrolling together with mouse jumps. No fresh gaze leaves
scrolling alone for that gesture. The HUD reports focus/replay failures; Windows
may deny foreground activation or input injection into elevated applications.
The low-level wheel hook uses a cached gaze target and performs no file access or
mutex waits. The hook and raw input run separately from preview rendering.
Wheel events generated by trackpad helper software are accepted even when Windows
marks them as injected. Only this app's uniquely tagged replay events are ignored.
The HUD counts incoming events, the synthetic subset, successful gaze targets,
and the most recent routing result to distinguish input detection from focus issues.

## Validation

### Posture-aware learning research (2026-10-05)

The live learner now logs synchronized head pose and eye origins at each gaze
jump, but its corrections still use the original spatial learner. The nearest
pose sample must be within 50 ms of the gaze timestamp and recently received;
invalid or stale samples remain missing. A read-only hardware probe is available:

```powershell
cargo run --example pose_probe
```

The installed ET5 runtime successfully provided gaze origin, head pose (position
and all three rotation axes), and user position guide. In a five-second probe,
head pose supplied 160 valid callbacks; gaze origin and position guide each
supplied 166 callbacks with 161 valid positions. The older normalized-eye-position
stream reported unsupported. Subscription success and valid callbacks establish
availability, not pose accuracy or its predictive value for calibration error.

Recommended experiment: record pose synchronized to the gaze sample used for a
jump, then compare (1) the current spatial map, (2) a regularized continuous
pose-conditioned residual model, and (3) a persistent bank of small correction
models with confidence-based switching. Share the general screen correction
across models; keep each posture's additional correction simple until enough
data supports spatial detail. All models retain dependence on both screen axes.
Allow recurring postures to emerge instead of forcing exactly six clusters.
Use both pose and click prediction error for model selection, resist uncertain
switches, and preserve old models when learning a new posture. This is a proposal,
not a demonstrated best algorithm for this tracker.

Evaluate chronologically: predict before training on each click, hold out entire
posture visits/sessions, and check independent target dots. Measure median/p90
error, correction distance, false switches, and recovery after changing posture.
Click labels can reflect deliberate gaze compensation and are not ground truth;
the short-correction gate also excludes many large errors. Existing logs omit
pose and cannot retrospectively establish posture-dependent accuracy.

Relevant primary research:

- [Sugano et al., ECCV 2008](https://doi.org/10.1007/978-3-540-88690-7_49):
  incremental gaze estimation from clicks with head-pose clustering. The webcam
  setting supports the approach, not direct accuracy claims for ET5 residuals.
- [WebGazer, IJCAI 2016](https://www.ijcai.org/Proceedings/16/Papers/540.pdf):
  interaction-supervised regularized regression.
- [FAZE, ICCV 2019](https://arxiv.org/abs/1905.01941): few-shot neural adaptation
  depends on pretrained image representations and meta-learning; a few local
  XY/click samples are not equivalent training data.
- [EyeO, 2023](https://arxiv.org/abs/2307.15039): users' deliberate gaze
  compensation can confound implicit calibration labels.
- [Pose-Robust Calibration, BMVC 2025](https://arxiv.org/abs/2508.10268):
  testing across varied head poses matters; its mobile/image setting differs
  from this desktop residual-calibration problem.

### Build and checks

For a guided, known-target comparison, close gaze-preview and run:

```powershell
cargo run --example posture_trial
```

Each round pauses on a full-screen posture instruction with no target visible.
Change posture and press Enter to confirm; Space cannot dismiss that screen.
Look at each dot, press Space, and hold gaze for 1.4 seconds. There are three
13-target rounds: upright, leaned back, then upright again. The first nine targets
of each of the first two rounds train the models. Four intermediate targets in
each round and the entire return visit are held out. Each prediction is recorded
before training; repeated gaze frames contribute to one robust target estimate,
not dozens of independent training labels. At least 20 matched gaze/head samples
and median fixation scatter below 70 pixels are required. Raw synchronized
samples, predictions, and per-round mean/median/p90 error are saved after each
capture in `recordings/posture-trial-*.json`. Q saves partial progress and exits;
Escape is reserved for the automation tool's stop shortcut. Captures cancel on
focus loss. Recorded round confirmations support protocol review; a completed
run remains `pending_review` until the participant confirms the procedure was
followed. Invalid runs belong in `recordings/invalid/` and must not be scored.

The trial compares fixed calibration, the existing local spatial learner, shared
affine ridge regression, continuous pose-conditioned ridge regression, and a
prototype bank of affine experts selected by head-pose distance. The bank retains
up to six experts; it is a simple experimental baseline, not the full proposed
probabilistic switching system. Features use 50 mm position and 0.2 radian rotation
scales, regularization is fixed before testing, and correction magnitude is capped
at 80 pixels. Experimental models never control the pointer. The spatial learner
receives the same known-target training labels without the normal click gates;
this is a controlled calibration comparison, not a simulation of normal clicking.
One short session cannot establish generalization to other days or postures.

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
node tests/learning_view.cjs
cargo run --release -- --probe 8
```

The initial eight-second hardware probe received 262 callbacks, of which 260
were valid, at approximately 33.1 Hz. This is the observed gaze-output rate for
this API session, not a camera frame-rate or end-to-end latency measurement.
The displayed Hz is computed from device timestamps over the recent one-second
arrival window; the UI requests repainting separately at a 16 ms timer interval.
The probe exits nonzero if it receives no valid samples. Because the executable
uses the Windows GUI subsystem, redirect its stdout when invoking it directly
if the caller does not capture output.

Checked: release build, Clippy, unit tests for negative-origin screen mapping,
stale/invalid samples, outlier rejection, sparse/drifting capture rejection,
held-out improvement/regression, curved-model selection, and full synthetic session
completion with a frozen model and disjoint validation targets; real gaze input
and full-screen entry/exit.
Physical unplug/replug recovery and multi-monitor switching remain untested.

## First milestone

Build a live gaze viewer and fixed-target validation session that measures sample
timing, tracking validity, calibration offset, and fixation scatter across the
screen. Use these measurements to guide filtering and calibration experiments.

The native UI uses Win32 and tiny-skia. `src/tobii.rs` isolates the dynamically
loaded vendor API from `src/preview.rs`, leaving room for a later independent USB
backend. No upstream repository has been forked or vendored.

## Later experiments

- Gaze-assisted selection with explicit keyboard or CharaChorder confirmation.
- UI-aware target selection and confidence visualization.
- Moving-target validation, accounting for eye-movement dynamics.
- Trajectory prediction, evaluated against recorded observations.

Higher sampling rates and better spatial precision are research questions, not
assumed capabilities unlocked by reverse-engineered protocol access.

## Starting references

These projects report relevant capabilities; none has been tested on this device
as part of this repository's setup.

- https://github.com/cmaybon/tobii-stream-engine — reference for the Stream Engine
  4.x Rust/C ABI used by our small dynamic adapter.

- https://github.com/Aetherall/tobiifree — direct USB protocol, gaze, calibration,
  and display configuration; experimental Linux/browser tooling.
  Its open issue #3 reports calibration sequencing and buffer problems; review
  the selected revision before using calibration writes.
- https://github.com/simonvc/tobii_ffg — direct USB gaze and Linux window focusing.
- https://github.com/njmill/tobii-linux — Linux gaze and in-band IR frame access.
- https://github.com/xanderscannell/ET5-Camera-Access — Windows RGB/IR camera access;
  the reported camera feeds are distinct from a high-speed gaze sample stream.

Keep locally collected gaze recordings and camera images out of Git by default.
