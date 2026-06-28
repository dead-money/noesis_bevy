---
name: prune-comments
description: Reduce comment verbosity in target files — remove restate-the-code comments and "thinking in comments", keep or sharpen comments that capture non-obvious WHY.
allowed-tools: Bash, Read, Edit, Grep, Glob
---

Prune comments in the target files. Default scope: $ARGUMENTS. If no argument is given, prune the files changed on the current branch (vs. `main`) plus unstaged/untracked files — i.e. `git diff --name-only main...HEAD` ∪ `git status --porcelain`.

The goal: every comment that survives earns its place by saying something the code cannot. Everything else is noise that rots and misleads.

The governing default is **no comment**. The burden of proof is on *retention*, not removal — a comment survives only by clearing a bar, never by failing to trip a removal rule. When you can't decide, the answer is delete: an absent comment costs a reader nothing; a marginal one costs them a read and may mislead. "It's not wrong" is not a reason to keep a comment.

**Zero is a valid result.** If a file's comments all earn their place, leave it untouched and report "no changes." Do not lower the bar to find work. Do not delete a load-bearing WHY to feel productive. A run that touches nothing is a correct run when the comments are already disciplined.

## Rules

### Remove

- **Restate-the-code.** A comment that paraphrases the next line. `// increment counter` over `i += 1`. `// loop over batches` over `for batch in batches`. `// return early if none` over `let Some(x) = x else { return };`. The identifier and structure already say it.
- **Thinking in comments.** Running narration of the author's decision process: "we tried X but switched to Y", "TODO: maybe refactor this later", "consider whether this should be...", "first we do A, then we do B, finally C". Section headers that recap the next 5 lines. Planning prose that belonged in a PR description.
- **Change-history annotations.** "was `bool`, now an enum", "added for the blit pass", "previously polled `Time<Real>`", "renamed from foo". That's what git is for; the diff and commit message carry it, the source must not.
- **Phase / task / caller bookkeeping.** `(Phase 5.C)`, `// Phase 4.D.3`, "used by `bake.rs`", "fixes #1234", "part of the binding-bridge work". The codebase is built in phases; restating the phase on individual items is decoration that rots when the phase ends, the caller is renamed, or the issue closes. Belongs in commit messages and PR descriptions. The one exception is in Keep below — a phase marker that encodes a real removal trigger.
- **SDK-mirror / port annotations.** "mirrors `Shader.140.frag`", "matches `GLRenderDevice`", "ports the IntegrationGLUT order". This crate is authored *against* the SDK reference impls by design; restating that on each identifier is noise and the citation rots when the SDK moves. Drop it — unless the reference is the *anchor* for a non-obvious correctness note (see Keep).
- **Banner / separator comments.** `// ===== HELPERS =====`, `// --- private methods ---`, ASCII-art dividers. The codebase's `// ── Section ──` headers are fine *only* when they label a genuine region of a long file and say something the next identifier doesn't — a bare divider over one function is noise. If a file is so long it needs banners to navigate, the fix is to split the file.
- **Commented-out code.** Delete it. Git remembers. The only exception: a single line documenting a known-broken alternative *with* a one-line reason it's disabled (e.g. `// view.SetProjectionMatrix(...)  // disabled — GL ortho culls children`).
- **Stale / contradicted comments.** Any comment whose claim disagrees with the current code. Either fix it or remove it; never leave the lie. (The thread-ownership and sRGB/linear invariants are load-bearing — a comment that contradicts them is actively dangerous.)
- **Redundant docstrings.** Multi-paragraph `///` blocks that mostly restate the signature ("Takes a `&Batch` and draws it"). Collapse to one short line capturing the contract, or drop entirely if the signature already does.
- **Trivial field/parameter comments.** `size: UVec2, // the size`. `queue: RenderQueue, // render queue`. If the name is bad, fix the name; don't paper over it.
- **Closing-brace labels.** `} // end of if`, `} // impl WgpuRenderDevice`. The editor folds; the indentation shows scope.

### Keep (and sharpen if vague)

Fitting one of these categories makes a comment *eligible* to survive — it does not make it survive. The comment must also carry a WHY a competent reader of *this code* would actually miss. A category match over an obvious-anyway fact is still a delete.

- **Non-obvious WHY.** A choice that would surprise a reader: why this algorithm over the textbook one, why this loop bound, why this order of operations matters. (E.g. not calling `View::SetProjectionMatrix` because a GL-style ortho makes Noesis's visibility pass cull children — surprising, load-bearing, keep.)
- **Hidden constraints / invariants.** "Must drop before the shutdown guard." "Runs on the render-app thread — `View`/`Renderer` are `!Send`." "Provider map never crosses the main↔render boundary." Anything a future editor could violate by mistake. The architectural invariants (two-crate `unsafe` split, render-app thread ownership, no `SetProjectionMatrix`, sRGB intermediate) are exactly this kind of anchor.
- **Units / encoding.** `secs_f64`, `sRGB bytes in an Rgba8Unorm target`, `premultiplied alpha`, `row-major Matrix4`. A bare number or buffer whose unit/encoding isn't obvious from its type earns a comment.
- **Workaround citations.** A comment paired with a concrete external reference: a Noesis SDK quirk, a Bevy/wgpu behavior, an SDK header path, a `TODO.md` entry. The citation is the value — bare "// workaround" is not. (E.g. the `CachedFontProvider` scan-once gotcha; the Bevy 0.18 `Time<Real>` non-extraction.)
- **Subtle correctness anchors.** "Sample through the sRGB alias so the byte round-trips instead of double-encoding." "Don't use `Time<Real>` — Bevy 0.18 doesn't extract it; reads 0.0 forever." Things tests can't easily catch but reviewers reliably get wrong.
- **Public API contract.** A short `///` / `//!` line stating the contract (preconditions, threading, lifetime, units). Keep it one line where possible. Drop the WHAT (the signature has it); keep the WHY/INVARIANT.
- **Scope-limited phase markers** *only* when they encode a real removal trigger, e.g. `// PHASE-9-REMOVE: intermediate + blit, once PipelineCache keys on format`. A bare "(Phase 5.B)" gets cut.

## Process

1. Resolve the file set from `$ARGUMENTS` (or the default git-derived set above). Exclude generated/vendored/external trees: anything under `target/`, the SDK (`$NOESIS_SDK_DIR`), the gitignored `assets/Data` / `assets/Fonts` symlinks, and any file marked generated.
2. Read each file fully before editing — context matters; a comment near the top may be load-bearing for code at the bottom.
3. For each comment, classify against the rules above. To keep one, state its WHY/invariant to yourself in a single clause. If that clause is just a paraphrase of the code, or you reach for "context" / "clarity" / "it helps" — delete. Uncertainty resolves to deletion, not to a reworded survivor. Only a comment you can name a concrete reason to keep gets rewritten; the rest go.
4. Apply edits with `Edit`. Do not reflow unrelated code, do not rename, do not reorder. Comments only.
5. After editing, report a per-file tally: `src/render.rs — removed 7, rewrote 2, kept 4`. No diff dumps; the user can read the diff themselves.
6. Do **not** run formatters, build, or tests. Comment-only edits don't change behavior; running CI here just adds latency and noise. If the user wants verification, they'll ask.

## Tone for rewrites

A kept comment is worth keeping *and* worth shortening. Sharpening is part of the prune, not a separate nicety — a verbose survivor is a half-done removal.

- A fragment beats a sentence. Cut to the surprising token and stop. No subject, no verb, no closing punctuation if they earn nothing.
- One line. A citation may take a second line; nothing else may. If the WHY won't fit on one line, the comment is carrying explanation that belongs in the commit/PR.
- Lead with the constraint or reason, not "This function...". Drop the WHAT the signature already states; keep only the WHY/invariant.
- No hedging ("maybe", "probably", "I think"). If it's uncertain, it isn't worth a comment.
- No first person, no audience address ("note that...", "as you can see..."). Just state the fact.

Worked sharpenings — each cuts the restated WHAT and keeps only the surprising part:

- `// We sample through the sRGB alias view here because the ViewTarget re-encodes linear→sRGB on write, so reading the raw bytes as linear would double-encode and wash out ~40% bright` → `// sRGB alias: round-trips the bytes instead of double-encoding`
- `// This must run on the render-app thread because View and Renderer are !Send and live as non-send resources` → `// render-app thread only — View/Renderer are !Send`
- `// Keep our own Instant because Bevy 0.18 doesn't extract Time<Real> to the render world, so it reads 0.0 forever and animations never advance` → `// own clock: Bevy 0.18 doesn't extract Time<Real> (reads 0.0)`

## What this skill is not

Not a refactor pass. Not a rename pass. Not a "improve the code" pass. If pruning a comment reveals that the code itself is unclear, **leave the code alone** and either keep a sharpened comment or flag it in the final report ("`render::is_linear_float` reads obscurely without its old comment — consider renaming in a separate change"). The user decides whether to act on that.
