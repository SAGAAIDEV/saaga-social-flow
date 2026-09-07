# Schedule → analytics → reflect

Queue posts through Buffer, pull performance, and rewrite prompts. Do not skip the ledger.

Pipeline today: Draft → Render → Post → **Distribute** (S3) → **Schedule** (Buffer: Build Plan, then Queue).

> Every GraphQL shape below was introspected against the live api. Where an earlier
> draft of this doc guessed (`assets: [{source, type: VIDEO}]`, `MutationSuccess`,
> uppercase enums, looking a post up by text hash), the guess was wrong and the
> corrected form is what the code sends. Trust this file over any memory of it.

Distribute writes `{root}/distribute/vN/links.json` (public URLs). Posts live in `{root}/posts/vN/posts.json`. Prompt overlays already load from `{root}/prompts/{prompt_id}.txt`.

## Rule

**Queue first. Agent later.** A Rig extractor proposes a typed plan. You tap Queue. Buffer `addToQueue` picks slots.

Do not give a Rig agent Buffer tools until a few weeks of `analytics.jsonl` exist. Without numbers it dumps every chapter everywhere at once.

## Buffer API

- **Endpoint**: `https://api.buffer.com/graphql`
- **Auth**: `Authorization: Bearer <api_key>` (get key at https://publish.buffer.com/settings/api)
- **Mutation**: `createPost(input: CreatePostInput!)` — pushes into Buffer's native queue per channel. No custom scheduling needed; Buffer drains at the channel's configured schedule slots.
- **Key input fields** (required: `channelId`, `mode`, `schedulingType`):
  - `channelId: ChannelId!` — which IG/TikTok/YouTube channel
  - `text: String` — caption
  - `mode: ShareMode!` — always `addToQueue`. Enum is lowercase camel: `addToQueue`, `customScheduled`, `shareNext`, `shareNow`
  - `schedulingType: SchedulingType!` — `automatic` | `notification`. Reminder-only channels (`Channel.metadata.defaultToReminders`) need `notification`
  - `dueAt: DateTime` — only set if `mode: customScheduled`
  - `assets: [AssetInput!]` — `AssetInput` is `@oneOf` keyed by media kind: `[{ video: { url: "https://s3.../video.mp4", metadata: { title } } }]`. The field is `url`, **not** `source`, and there is no `type: VIDEO`
  - `metadata: PostInputMetaData` — per service, all enum values lowercase:
    - `instagram: { type: "reel", shouldShareToFeed: true }` — both are non-null, `firstComment` optional
    - `tiktok: { title, isAiGenerated }` — both nullable
    - `youtube: { privacy: "public", madeForKids, notifySubscribers, title }` — **no** `type` field
    - `facebook: { type: "post" }` — `PostTypeFacebook!` is required
    - `linkedin`, `twitter`, `bluesky` take metadata but we send none

## Cadence — delegated to Buffer, not implemented here

The intended rhythm: YouTube longform is the hub, verticals are clips released over
days, not a simultaneous blast. Short-form (IG Reels + TikTok) wants **3 chapters
per day** — morning (8a), noon (12p), night (8p).

**None of that is scheduling code, and deliberately so.** Every item goes out as
`mode: addToQueue` in one burst; the channel's own Buffer posting schedule decides
when each one fires. Configure the slots in Buffer, not here. `Channel.postingSchedule`
comes back from the channels query if a future version wants to show them.

A per-platform cadence table (LinkedIn "next business day", one chapter per day)
would need `mode: customScheduled` plus `dueAt`, which nothing in this stage sets.

## Phase 1 — Queue

The queue is **append-only**. We never remove or reorder. Buffer's native queue handles execution order. Every `createPost` with `mode: addToQueue` goes to the end of the line.

Two buttons, two jobs — `src/schedule/`:

1. **Build Plan** (`plan.rs`) is deterministic, not an extractor. No LLM, no network beyond the channels query. It reads `posts.json`, `distribute/vN/links.json`, the live channel list and `{root}/schedule.jsonl`, and writes `schedule/vN/schedule.json`.
2. **Queue** (`mod.rs::run_queue`) sends exactly the ready items of the saved plan. The ledger — never the plan — decides what is already live, so pressing Queue twice sends nothing twice.

Channels must be asked for by organization:

```graphql
query Channels($input: ChannelsInput!) {
  channels(input: $input) {
    id name displayName service type timezone isDisconnected isQueuePaused
    postingSchedule { day times paused }
    metadata {
      ... on InstagramMetadata { defaultToReminders }
      ... on TiktokMetadata { defaultToReminders }
      ... on YoutubeMetadata { defaultToReminders }
    }
  }
}
```

`ChannelsInput.organizationId` is required. With `BUFFER_ORG_ID` unset, resolve it from `query { account { organizations { id name } } }` and take the first — the code names every organization on stderr when there is more than one, because picking the wrong workspace looks exactly like "no channel connected".

Channel ids are resolved at runtime, never hardcoded: the account has two Twitter profiles and both a LinkedIn page and profile, so a 1:1 platform→channel map loses one. `youtube_shorts` is not a channel; it rides the youtube channel.

Writes `SchedulePlan` — one item per (video, platform), carrying the exact payload Queue will send:

```json
{
  "project": "vd-42-my-video",
  "version": 3,
  "items": [
    {
      "video_id": "longform",
      "platform": "youtube",
      "channel_id": "…",
      "channel_name": "SAAGA Solve",
      "url": "https://…/longform.mp4",
      "text": "…",
      "title": "…",
      "mode": "addToQueue",
      "scheduling_type": "automatic",
      "needs_approval": false,
      "metadata": {
        "youtube": {
          "privacy": "public",
          "madeForKids": false,
          "notifySubscribers": true,
          "title": "…"
        }
      },
      "reason": "hub video",
      "prompt_id": "posts.social",
      "copy_hash": "30505c9acb242fe7"
    },
    {
      "video_id": "chapter-02",
      "platform": "instagram",
      "url": "https://…/vertical/chapter-02.mp4",
      "text": "…",
      "mode": "addToQueue",
      "scheduling_type": "automatic",
      "metadata": { "instagram": { "type": "reel", "shouldShareToFeed": true } },
      "reason": "vertical chapter",
      "copy_hash": "…",
      "skip": "already queued 2026-08-14T23:12:04Z"
    }
  ]
}
```

`metadata` lives in the plan on purpose: `privacy: public` and `notifySubscribers: true` are irreversible, and a review step that hides them is not a review step. `skip` present ⇒ the item is **not** queueable, and the string says why (no url, no channel, channel disconnected or paused, already queued).

Queue executes each ready item with `createPost`. `PostActionSuccess` implements no interface and there is no `MutationSuccess` — asking for one is a validation error. All six error members implement `MutationError`:

```graphql
mutation CreatePost($input: CreatePostInput!) {
  createPost(input: $input) {
    ... on PostActionSuccess { post { id dueAt status } }
    ... on MutationError { message }
  }
}
```

Variables per item:

```json
{
  "input": {
    "channelId": "…",
    "text": "…",
    "mode": "addToQueue",
    "schedulingType": "automatic",
    "needsApproval": false,
    "aiAssisted": true,
    "assets": [{ "video": { "url": "https://s3…/video.mp4", "metadata": { "title": "…" } } }],
    "metadata": { "instagram": { "type": "reel", "shouldShareToFeed": true } }
  }
}
```

The mutation returns the post id **directly**. Never look a post up afterwards by channel + text hash: `Channel.linkShortening` rewrites text server-side, so text matching is broken by design.

After each `createPost`, append `{root}/schedule.jsonl` — at the project root, outside any version, so dedupe spans versions:

| Field | Why |
|---|---|
| `id` | row id |
| `buffer_post_id` | join key for analytics |
| `project`, `version` | take |
| `video_id`, `platform` | asset |
| `url` | what Buffer fetched |
| `prompt_id` | which prompt wrote the copy |
| `copy_hash` | detect edits vs generated |
| `queued_at` | when we sent it |

### Dedupe

The key is `{video_id}:{platform}:{copy_hash}`. `copy_hash` is an explicit FNV-1a/64
over the text + title (`copy.rs`, pinned by known-answer tests) — **not** `DefaultHasher`,
whose algorithm std is free to change between releases. A digest that drifts would
re-queue every project's back catalogue.

Consequences worth knowing:

- Same copy, already in the ledger ⇒ skipped, in the plan *and* again at send time.
- **Regenerated copy is a new item by design** (that is how an edited caption gets
  posted). The video is still live under the old words, so the plan's `reason` carries
  a `warning: already queued as <post id>` line instead of silently duplicating.
- An unreadable ledger is an error, never an empty list. Silently "no rows" means
  no dedupe, which means re-posting everything.
- A `createPost` that succeeds and whose ledger append then fails is reported as
  `UNRECORDED` on stderr and in the status line: it is live but invisible to the
  next plan, and it is the one state that needs a human.

Schedule tab: preview plan → **Queue**. No auto-fire until you say so.

## Phase 2 — Analytics

Per-post metrics come from the `Post` type. `aggregatedPostMetrics` takes
`{ organizationId, startDateTime, endDateTime, channelIds, tags }` and has **no**
`postIds` argument — it cannot answer "how did post X do":

```graphql
query Post($input: PostInput!) {
  post(input: $input) {
    id status sentAt metricsUpdatedAt
    metrics { name type unit value }
  }
}
```

`PostInput` is `{ id: PostId! }`. `PostMetric` is `{ name, type, unit, value, description }`
with `unit: count | percentage`. There is no flat `engagement` metric — `engagementRate`
is a percentage. Pull 48h and 7d after `queued_at`, keyed by `buffer_post_id`.

YouTube CTR / retention later via existing YT OAuth.

Write `{root}/analytics.jsonl`:

| Field | Why |
|---|---|
| `buffer_post_id` | join to schedule |
| `pulled_at`, `window` | 48h / 7d |
| `metrics` | the raw `name/type/unit/value` list, not a flattened triple |
| `platform` | slice |

Store the metric list as it arrives; flattening to impressions/engagement/clicks
throws away the metrics that differ per platform. Without the join, reflection is guesswork.

## Phase 3 — Reflect

One extractor (`reflect.prompts`), not a chat loop.

Input: last N analytics rows + the `posts.social` preamble that produced them + optional human edits (approved titles, saved post fields). There is no `schedule.plan` preamble — planning is deterministic code, not a prompt.

Output:

```json
{
  "keep": ["hooks under 80 chars on Twitter"],
  "drop": ["generic CTA on LinkedIn"],
  "rewrite": [
    { "prompt_id": "posts.social", "preamble": "…" }
  ]
}
```

Write overlays to `{root}/prompts/{prompt_id}.txt`. Next Post / Schedule generate already picks those up (`src/agent/prompt.rs`).

Optional: `{root}/memory/playbook.json` (what hook length / CTA / tag count won) so the next schedule extractor can read a short memory without the full ledger.

## Prompt ids

| id | Stage |
|---|---|
| `notes.slide_deck` | Notes (exists) |
| `titles.chapter_cards` | Render titles (exists) |
| `posts.social` | Post generate (exists) |
| `reflect.prompts` | This loop (phase 3, not built) |

Only ids a stage actually loads belong here — an id with no `prompt::load` call behind
it is a file the user can write that nothing will ever read.

## What not to do

- Free-form agent posting to Buffer in v1
- file.io or any URL that expires before the queue fires (S3 is the host)
- Reflecting on copy without `buffer_post_id` on both ledgers
- Skipping Distribute — Buffer only fetches public URLs
- Trusting the plan file over the ledger for "has this been posted"

## Build order

1. ~~Plan + Queue buttons + `schedule.jsonl`~~ (done — needs Distribute links)
2. Analytics pull → `analytics.jsonl` (see the corrected `post(input:)` query above)
3. Reflect extractor → prompt overlays
4. Only then: agent tools (`list_queue`, skip a slot) if the queue needs steering
