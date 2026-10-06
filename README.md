# EyeTracking

Personal eye-tracking experiments, starting with a Tobii Eye Tracker 5 on Windows.
The first native Rust gaze preview is implemented and tested with the connected
ET5 and the installed Tobii Stream Engine 4.25.0.3 on Windows.

## Run

```powershell
Set-Location C:\Users\andre\RustroverProjects\EyeTracking
cargo run --release
```

Or run `target\release\gaze-preview.exe` after building. The preview uses the
installed DLL at `C:\Program Files\Tobii\Tobii EyeX\tobii_stream_engine.dll`.
An absolute `TOBII_STREAM_ENGINE_DLL` environment variable can override that path.
No SDK download or import library is required. The adapter currently accepts
Stream Engine major version 4; other major versions need an ABI review.

The mint ring shows the latest valid gaze position, with a short fading trail.
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
| Esc | Cancel calibration, leave results, or close the preview |

## Calibration experiment

Select the tracker’s configured screen using M, then press C. Look at the bright
dot and press Space. Keep looking until it moves: each capture allows 0.8 seconds
to settle and collects for 1.6 seconds. Press Space separately at each new dot.
The gaze marker is hidden throughout calibration to avoid chasing feedback.
Losing window focus pauses the capture; changing display geometry cancels the run.

The first 15 dots fit a correction to the existing gaze output. Per-axis median
absolute deviation (MAD) rejects samples with modified Z scores over 3.5, with a
2-pixel MAD floor for quantization. The remaining samples are averaged. Rejection
does not depend on closeness to the target. Captures need at least 25 retained
samples, 65% valid samples, and 70% retained among valid samples. Excess scatter
(2.5% of display diagonal) or drift (2% of diagonal, first vs last third) requests
a retry. These thresholds are experimental, not a certified fixation detector.

An affine residual map corrects offset, scale, and skew. A regularized quadratic
map is selected only if leave-one-target-out training RMS improves by at least
10%. Models with folding, extreme stretching, or excessive correction are rejected.
The map is frozen before collecting 8 new validation dots, at different positions.

Results show original and corrected mean-target error, the worst target, and RMS
error across ALL valid validation samples (including centroid outliers), weighted
equally by target. White rings are validation targets, orange rings their original
means, and mint dots the corrected means. Grey clouds are retained fitting samples;
red dots are rejected fitting samples.

Correction turns on automatically only when validation mean-target error improves
by over 10% and 2 pixels, all-valid-sample RMS improves, at least 6 of 8 targets
improve, and the worst target does not regress by more than 10%. A toggles the map
for comparison, including an experimental override when the checks do not pass.
This is a short within-session check, not evidence of long-term calibration quality.

Completed runs save observations, rejected points, means, model coefficients,
display geometry, and validation metrics in `recordings/calibration-*.json`, excluded
from Git. The correction is local to this app and only applies on the same display
name/geometry. The latest valid saved report is restored on startup; redo calibration
if posture or Tobii calibration changes. Tobii's calibration is never overwritten.

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

Run `cargo run --release -- --mouse` to start enabled, or press W in the preview.
Turn off Tobii Experience's own **Warp on mouse move** to avoid two controllers.
After 300 ms without pointer activity, the very first nonzero raw mouse/trackpad
delta triggers one jump. There is no minimum travel, speed, direction, gaze-distance,
or fixation threshold. Continued movement provides normal fine control. The next
300 ms pause rearms it. Button presses, dragging and scrolling reset that pause.

A click-through status panel shows ARMED, FINE CONTROL, PAUSED or NO GAZE, plus
learning and recording status. It starts hidden; **Ctrl+Alt+F6** shows or hides it
without changing gaze control, the dot, or learning. The panel also includes
successful and missed jump counts. **Ctrl+Alt+F8** pauses/resumes from any app;
closing the preview stops the controller. It runs on a separate input thread.
A tiny gaze dot with a one-pixel white rim stays above desktop apps,
including while the preview is minimized or mouse jumps are paused. Its center
uses the same calibrated position as a jump. The dot is nine physical pixels
wide, takes no focus and passes clicks through. It hides on tracking loss or
during calibration, so it never shows an old position as live gaze.
Black means the next movement is armed to jump. Orange means movement will stay
in fine control, including the 300 ms rearm interval, held buttons, and paused
mouse assistance. Color refreshes every 16 ms, even if the gaze position is still.
**Ctrl+Alt+F7** shows/hides just the desktop dot from any app; mouse jumps,
gaze scrolling, and learning continue unchanged. The HUD shows Dot ON/OFF.
The dot starts visible each time the app launches.
Raw Input accepts precision touchpads with null device handles, and cursor warps
do not feed back as physical motion. The latest gaze sample must be valid and no
older than 200 ms. A missing sample consumes that movement attempt with a visible
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
are learning filters only; the first-motion jump still has no travel threshold.

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
