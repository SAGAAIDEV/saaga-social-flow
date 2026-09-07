# Reflect — read everything, rewrite the prompts

A Rig extractor that reads every generative step's inputs and outputs, what a
human changed afterwards, and how the result performed — then proposes new
preambles. Output is a **proposal**, never a write.

Pipeline today: Draft → Render → Post → Distribute → Schedule → Analytics →
**Reflect**.

## Rule

**Propose, don't apply.** A prompt overlay changes every future generation in the
project the moment it lands. It goes through the same gate as Queue: see the
diff, approve it, then it's written.

## What exists to read

| Source | Path | Carries |
|---|---|---|
| LLM trace | `{root}/llm.jsonl` | `prompt_id, step, model, provider, **preamble**, prompt, output` |
| Schedule ledger | `{root}/schedule.jsonl` | `buffer_post_id, video_id, platform, prompt_id, copy_hash` |
| Analytics | `{root}/analytics.jsonl` | `buffer_post_id, window, metrics[], sent_at` |
| Posts | `{root}/posts/vN/posts.json` | the caption text itself |
| Titles | `{root}/titles/vN/titles.json` | `title` + `approved` |
| Plan | `{root}/schedule/vN/schedule.json` | `approved`, skip reasons |
| Overlays | `{root}/prompts/{id}.txt` | the write target |

`llm.jsonl` recording the **preamble** is what makes this possible at all: the
ledger knows which prompt text produced which output, so a rewrite can be
attributed later rather than guessed at.

### The join

Four hops, from a number back to the words that earned it:

```
analytics.jsonl  --buffer_post_id-->  schedule.jsonl
                                        |  prompt_id + copy_hash + video_id/platform
                                        v
                                     posts.json   (the caption text)
                                        |
                                        v
                                     llm.jsonl    (the preamble that wrote it)
```

## Three gaps

**1. `posts.social` is not traced.** — **closed.** `generate_posts` now builds an
`LlmStep` and `posts/mod.rs` writes it, so the ledger finally carries the preamble,
prompt and output of the step reflection most needs to read.

**2. Nothing records *which* preamble produced a queued post.** — **closed, by
versioning per project.** The builtin is v0; each applied overlay is v1, v2…
recorded in `{root}/prompts/versions.jsonl`. `prompt::resolve` returns the text
*and* its version, and that version travels: `LlmStep` → `posts.json` →
`PlanItem` → `schedule.jsonl` → the analytics join. So performance attributes to a
preamble rather than to a date, and reflection can read its own history.

An overlay whose hash is not in the ledger resolves as **unversioned** rather than
as the version it replaced — a hand-edited file must not be attributed to a prompt
that never ran. The hash is FNV-1a/64, fixed by specification, because std's
`DefaultHasher` is explicitly unstable across releases and this key is persisted.

A hand edit in the Post tab carries the attribution through unchanged. The edit is
the model's output plus a human's correction, and that delta is exactly the signal
Phase 1 reads.

**3. A free rewrite can break generation.** — **closed at the root.** The
`posts.social` builtin used to carry the platform rules *and* the JSON output
schema, and an overlay replaces the whole preamble, so a rewrite that forgot to
repeat the schema broke every future generation.

Posts now go through the Rig extractor like notes and titles, so the shape is a
`JsonSchema` enforced by the API and the schema prose is gone from the prompt
entirely. What remains in the preamble is only guidance — voice, platform rules,
length — which is exactly what reflection should be free to rewrite. No
contract/guidance split is needed once the contract is structural.

That also removed the lenient hand-rolled parser: an empty `content` used to
default to `""`, so a truncated response produced captions that looked valid all
the way to the Schedule tab. Empty posts are now dropped.

Still worth doing: **validate before applying** — run a proposed preamble against
real project context and require the result to parse before the overlay is
written. Structural schemas stop a rewrite breaking the *shape*; they do not stop
one that produces unusable copy.

## What it reads, and what it must not

Context budget is the real constraint. `llm.jsonl` holds full user prompts, which
are mostly *transcripts* — the largest and least useful part.

**Feed it:**
- every `preamble` (short, and the thing being optimised)
- every `output` (the generated titles/notes/captions)
- generated-vs-final deltas: titles where `approved` flipped, captions edited by
  hand, plan items never approved
- analytics rows joined to their caption text and `prompt_id`
- model and provider per step

**Don't feed it:** raw transcripts from `prompt`. Summarise as "chapter N,
~M words" and let the outputs speak. This is the difference between a request
that fits and one that doesn't.

## The extractor

`reflect.prompts`, following `agent/titles.rs` — a typed Rig extractor via
`agent/extract.rs`, so its output is schema-validated rather than parsed.

```json
{
  "keep": ["hooks under 80 chars on Twitter"],
  "drop": ["generic CTA on LinkedIn"],
  "evidence": [
    { "claim": "…", "post_ids": ["…"], "metric": "views", "delta": "+180%" }
  ],
  "rewrite": [
    { "prompt_id": "posts.social", "section": "guidance", "preamble": "…", "why": "…" }
  ]
}
```

`evidence` is not decoration. A recommendation with no post ids behind it is the
model's prior, not a finding, and should be shown as such in the review.

## Phases

**Phase 1 — reflect on edits only.** No analytics needed, works today. Compares
generated vs approved titles, generated vs edited captions, and which planned
items a human declined to approve. That is real signal about the prompt, available
before a single post has gone out.

**Phase 2 — reflect on performance.** Adds the analytics join. Needs posts that
have sent *and* matured to 7 or 30 days, so it is weeks out from the first queue.

**Phase 3 — measure the rewrites.** Needs gap 2 closed. Compares metrics for posts
written before and after a preamble change, so reflection reports on itself.

**Cross-project** is the natural extension, same argument as the analytics work
queue: one project's twelve captions is a thin sample, twenty-seven projects is
not. Per-project first, since it matches the file layout.

## The review gate

A Reflect tab mirroring Schedule:

1. **Reflect** — runs the extractor, writes `{root}/reflect/vN/reflect.json`
2. Review — keep/drop/evidence, and a **line diff** of each proposed preamble
   against what is live, with unchanged runs elided. Reviewing a rewrite by
   reading the whole new text is how a subtle deletion gets approved: forty lines
   look right, and the one rule that quietly vanished is invisible. Each row
   leads with the scale of the change — "+1 −1 lines" and "+40 −38 lines" are
   very different things to be approving.
3. **Apply Selected** — writes only the ticked overlays to `{root}/prompts/{id}.txt`

Overlays are backed up to `{root}/prompts/history/{id}.{stamp}.txt` on every write.
A prompt is the program's behaviour; reverting must not depend on the model having
been right.

## What not to do

- Auto-apply an overlay. Approval is the whole point.
- Feed raw transcripts — that is the context budget gone for no signal.
- Let it rewrite the output schema (see gap 3).
- Recommend from one project's data and call it a pattern.
- Report a recommendation without the post ids behind it.

## Build order

1. ~~Trace `posts.social`~~ (gap 1) — done
2. ~~Prompt versioning through the ledger~~ (gap 2) — done
3. ~~Make the output schema structural~~ (gap 3) — done, via the extractor
4. ~~`reflect.prompts` extractor + Reflect tab~~ — done
5. ~~Validate-then-apply~~ — done; a rewrite needs a tick *and* a passing run
6. Cross-project reflection, and self-measurement (Phase 3) once posts have matured

### Still to do

- **Phase 3 self-measurement.** Everything needed is recorded — `prompt_version`
  reaches `schedule.jsonl` and the analytics join — but nothing yet groups metrics
  by prompt version to answer "did v2 beat v1?".
- **Cross-project.** One project's captions are a thin sample; the corpus builder
  takes a session, so widening it is mechanical.
- **Validation beyond posts.** A titles or notes rewrite is applied on the review
  alone and says so rather than implying it was tested.
