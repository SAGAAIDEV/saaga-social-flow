# Posting schedule: a calendar per category, on AWS

Drafted 2026-10-08. Not built. This is the plan the socials archive plan
defers to: its decision 15 (events and the lambdas that act on them) and the
cadence it leaves to "its own plan" under Calendars.

## Context

**Why Buffer is not enough.** Today the Schedule stage sends every approved
item to Buffer with `createPost(mode: addToQueue)`, and Buffer drains each
channel at that channel's posting slots (`docs/schedule-loop.md`). Slots belong
to a channel, and every category posts through the same channels, so every
category shares one rhythm. A per-category schedule cannot be expressed there.

**What the app does now.**

| Step | Where | Notes |
|---|---|---|
| YouTube longform upload, thumbnail, playlist | the app, `src/publish/youtube.rs` | own Google OAuth client; shared `YOUTUBE_REFRESH_TOKEN` in `dev.sops.env` |
| Media hosting | `saaga-screencast-media/screencast/`, public | **expires at 30 days** |
| Social posts (LinkedIn page + profile, X, Instagram, TikTok, Facebook, YouTube Shorts) | Buffer, `src/schedule/` | channel ids resolved live; ledger `schedule.jsonl` |
| Analytics | Buffer, `src/analytics/` | joined on `buffer_post_id` |

**The AWS pattern already exists.** `saaga-internal-lambdas` holds the code
(one `lambda-<name>/` folder per function, Node 22 ESM, vitest at 85% or more,
secrets in sops `.env.dev.enc`, CI deploys on merge).
`saaga-terraform/lambdas-internal.tf` holds the function, triggers, IAM and
alarms. Everything is in the dev account only (`local.internal_lambdas_count`).
The internal bucket already sends every object write to EventBridge, and
`lambda-meeting-tickets` is the template for an S3 write that triggers a lambda.

## What Pipedream does and does not do

Checked against the Pipedream Connect docs on 2026-10-08.

- **Connect** runs the OAuth flow, stores the connected account (`apn_…`) and
  keeps it refreshed. It reports `healthy`, `dead`, `last_refreshed_at` and
  `next_refresh_at`.
- **The tokens can be read back** (`GET /accounts/{id}?include_credentials=true`)
  **only for accounts connected through our own OAuth client.** Pipedream calls
  this "bring your own app". With Pipedream's own clients, the API can only be
  reached through the **Connect proxy**, which inserts the token for us, or
  through Pipedream's prebuilt actions.
- The connection webhook carries no credentials. It names the account id.
- The development environment allows 10 external users, and the person
  connecting must be signed in to pipedream.com. Production needs the paid
  Connect plan.

This changes the plan to save refresh tokens to a database:

1. **Reading tokens needs our own developer app on every platform.** That
   means LinkedIn Community Management approval to post as the page, Meta app
   review for Instagram publishing, a TikTok audit (an unaudited app posts as
   private only) and Google verification for YouTube. Pipedream's own clients
   would let us skip those reviews.
2. **Two holders of one refresh token break each other.** Pipedream refreshes
   on its own schedule. X rotates the refresh token every time it is used, so
   whichever side refreshes second is left holding a dead token. A copy in our
   database is either stale or a second refresher.

**Recommendation: Pipedream is the only holder of tokens.** Our database
records which account posts what (`apn_…` per category and platform) and never
stores a token. At send time the publisher calls the platform through the
proxy. For a platform that needs our own client (for example because the
proxy cannot carry a video upload), it fetches a fresh **access** token just
before sending and never touches the refresh token. If the reason to store
tokens is to be able to leave Pipedream later: accounts connected through our
own client can export their credentials through the same API when that day
comes, so nothing needs to be mirrored continuously.

## Decisions

1. **DynamoDB is the calendar.** One row per post. The calendar is a view of
   the table, and nothing else decides when a post goes out.
2. **Cadence is per category, as configuration.** It lives in
   `socials/calendars/cadence.json` in the internal bucket: per category, per
   platform, the weekly slots in one timezone. People edit it now, and the
   Calendar tab edits it later. The versioned bucket keeps its history.
3. **Posts arrive as events.** When the app approves a schedule it writes one
   outbox file to S3. The S3 Object Created event is the trigger. There is no
   API Gateway, and the app needs nothing beyond the S3 put that Distribute
   already does.
4. **Slots are assigned on arrival.** Each item takes its category's next free
   slot for its platform and account. The order holds (the longform first,
   then chapter 1, then chapter 2), and so does any `not_before`.
5. **One timer per post.** Each post gets an EventBridge Scheduler one-time
   schedule, `at(…)`, in one schedule group per category. Only one lambda
   creates, moves or deletes schedules, and it reads the table's DynamoDB
   stream. So any change to a row (an enqueue, a drag on the calendar, a
   cancel) keeps its timer in step, and the table and the timers cannot drift.
6. **One queue per category.** Each schedule targets its category's SQS queue
   (`social-posts-<category>`), and each queue has its own DLQ. Pausing a
   category means disabling its event source mapping. Its failures land in its
   own DLQ and raise an alarm that names the category.
7. **The message is a pointer.** It is `{post_id, rev}`, not the post. The
   publisher re-reads the row and sends only if `rev` still matches and a
   conditional update moves the status from `scheduled` to `sending`. A stale
   timer left by a reschedule, or an SQS redelivery, then sends nothing.
8. **Delivery is an adapter chosen per account, starting with Buffer.** At the
   slot time the publisher calls Buffer `createPost(mode: shareNow)`. That
   gives per-category schedules on the channels we already have, with no app
   reviews and no Pipedream. After the spike, Pipedream replaces Buffer one
   platform at a time. Switching a platform is a change to a row, not a deploy.
9. **Accounts are a table.** The key is (category, platform and channel), and
   `*` stands for any category. A row holds the adapter and either a Buffer
   channel id or a Pipedream `apn_…` with its external user id. The same table
   works whether categories share accounts or each has its own. LinkedIn's
   page and profile are two rows, so the current fan-out carries over.
10. **YouTube longform stays in the app** for now: the upload, the thumbnail
    and the playlist. It shows on the calendar as a published row. YouTube
    Shorts go through the scheduler like every other vertical. Scheduling
    longform itself (`publishAt`, which `youtube.rs` already handles) is a
    later phase.
11. **Approval stays in the app.** Only approved items reach the outbox, and
    each carries the exact payload it will send, as `schedule.json` does today:
    a review step that hides `privacy: public` is not a review step.
12. **Results go back to the record.** The publisher writes the platform ids
    and the URL onto the row, and writes one file per post to the video's
    `distribution/published/<post_id>.json`, so concurrent posts never write
    the same object. The archive plan's `socials/calendars/YYYY-MM.json`
    becomes a snapshot that the stream lambda rewrites. The table remains the
    source of truth.
13. **A post's media has to outlive its slot.** `screencast/` expires at 30
    days. Enqueue holds any item whose slot falls after its media's expiry
    minus two days, gives the reason, and the calendar shows it. Later, once
    the archive keeps the finals, the publisher copies the final into the
    public bucket just before it sends.
14. **A newer version supersedes an older one.** When `vN+1` arrives, rows
    from older versions of the same video and platform that have not been sent
    yet become `cancelled` (`superseded by v3`). Rows already sent stay as they
    are.

## Flow

```
saaga-social-flow (Mac)
  Schedule → Approve → PutObject socials/<cat>/<project>/videos/<video>/distribution/outbox/vN.json
                                   │  S3 → EventBridge: Object Created, socials/*/distribution/outbox/*.json
                                   ▼
                        lambda-social-enqueue   (reserved concurrency 1)
                          reads calendars/cadence.json and the accounts table
                          puts rows: status=scheduled, at=<slot>, rev=1
                                   ▼
                        DynamoDB social-posts ──stream──▶ lambda-social-calendar
                                                            ├─ upserts or deletes schedule post-<id>
                                                            │    in group social-<cat>, at(<slot>)
                                                            └─ rewrites calendars/YYYY-MM.json
                        EventBridge Scheduler ── at slot ──▶ SQS social-posts-<cat> (+ DLQ)
                                                                   ▼
                                                        lambda-social-publish
                                                          ├─ scheduled → sending (conditional, rev)
                                                          ├─ adapter buffer:    createPost shareNow
                                                          ├─ adapter pipedream: Connect proxy, or fresh access token
                                                          ├─ row → published | failed
                                                          └─ distribution/published/<post_id>.json
```

## Data

**Outbox**, written by the app. Its key follows the archive layout and the
archive's slug rule, even before archive sync exists:

```json
{
  "outbox_version": 1,
  "category": "agents",
  "project": "support-agents-that-file-tickets",
  "video": "the-whole-ticket-loop",
  "recording_id": "2026-10-09_15-00-00",
  "version": 2,
  "not_before": "2026-10-14T16:00:00Z",
  "items": [
    {
      "item_id": "chapter-02/instagram",
      "order": 2,
      "platform": "instagram",
      "media": [{ "kind": "video", "url": "https://saaga-screencast-media.s3…/chapter-02.mp4",
                  "expires": "2026-11-08" }],
      "text": "…",
      "metadata": { "instagram": { "type": "reel", "shouldShareToFeed": true } },
      "prompt_id": "posts.social",
      "copy_hash": "30505c9acb242fe7"
    }
  ]
}
```

**`social-posts`**: one row per post, and per account where a platform fans
out.

| Attribute | |
|---|---|
| `post_id` (PK) | a hash of project/video/version/item/account, so re-reading the same outbox inserts nothing (`attribute_not_exists`) |
| `category`, `project`, `video`, `version`, `item_id`, `platform`, `account_key` | |
| `at` | UTC ISO time of the slot; `month` = `YYYY-MM` of it |
| `status` | `scheduled` · `held` · `sending` · `published` · `failed` · `cancelled` |
| `rev` | increases on every change to the time or the payload |
| `payload` | text, media, metadata, exactly as in the outbox |
| `result` | platform post id, URL, `sent_at`; the Buffer post id when Buffer sent it |
| `reason` | why it is held, failed or cancelled |
| `outbox_key` | the S3 key it came from |

GSIs: `by-category` (`category`, `at`) for a category's calendar, `by-month`
(`month`, `at`) for the snapshot and the month grid, `by-account`
(`account_key`, `at`) for slot collisions when categories share an account.
Stream: `NEW_AND_OLD_IMAGES`. PITR on, because this table is a record.

**`social-accounts`**: PK `category` (`*` = any), SK `platform#channel`
(`linkedin#page`, `linkedin#profile`, `twitter#main`). Each row holds
`adapter` (`buffer` | `pipedream`), `buffer_channel_id` or
`pipedream_account_id` + `external_user_id`, `label` and `enabled`. It holds
no tokens.

**`socials/calendars/cadence.json`**:

```json
{
  "timezone": "America/Los_Angeles",
  "min_gap_minutes_per_account": 120,
  "categories": {
    "agents": {
      "linkedin":  [{ "days": ["tue", "thu"], "times": ["08:00"] }],
      "twitter":   [{ "days": ["mon", "wed", "fri"], "times": ["09:00"] }],
      "instagram": [{ "days": ["daily"], "times": ["08:00", "12:00", "20:00"] }],
      "tiktok":    [{ "days": ["daily"], "times": ["08:00", "12:00", "20:00"] }]
    },
    "education": { "…": "…" }
  }
}
```

Slots are resolved in the cadence's timezone, which handles daylight saving,
and stored in UTC. The default short-form rhythm in `schedule-loop.md` (three
a day at 8a, noon and 8p) is the starting point for each category, not a rule
in code.

## Terraform (saaga-terraform, from `main`)

Prerequisites from the archive plan, Terraform steps 1–4: add `socials` to the
internal folders, set `bucket_key_enabled`, and check that the dev role can use
the KMS key.

New file `social-scheduler.tf`, dev only like `lambdas-internal.tf`:

1. `local.social_categories`: the agreed slugs plus `uncategorized`. Adding a
   category later is one line here.
2. DynamoDB tables `social-posts-dev` (stream, PITR, the three GSIs) and
   `social-accounts-dev`, both pay per request.
3. Per category (`for_each`): SQS `social-posts-<cat>` and
   `social-posts-<cat>-dlq` (redrive after 3 receives, visibility timeout 6×
   the publisher's timeout), an `aws_scheduler_schedule_group` `social-<cat>`,
   and a CloudWatch alarm when the DLQ holds anything, to `module.sns`.
4. One IAM role that Scheduler assumes, allowed `sqs:SendMessage` on the
   category queues.

In `lambdas-internal.tf`, by the existing convention (copy the
`lambda_meeting_tickets` block: dummy zip, `ignore_source_code_hash`, a `dev`
alias):

| Lambda | Trigger | Timeout | IAM beyond basic execution |
|---|---|---|---|
| `lambda-social-enqueue` | EventBridge rule: Object Created, `socials/*/distribution/outbox/*.json`; reserved concurrency 1 so two outboxes cannot take the same slot | 60 s | S3 Get on `socials/*`; DynamoDB Put, Update, Query on both tables; KMS via S3 |
| `lambda-social-calendar` | DynamoDB stream on `social-posts` | 60 s | `scheduler:Create/Update/DeleteSchedule` in the `social-*` groups; `iam:PassRole` on the Scheduler role; S3 Put on `socials/calendars/*` |
| `lambda-social-publish` | one SQS event source mapping per category queue: batch size 1, `maximum_concurrency` 2, `ReportBatchItemFailures` | 900 s | DynamoDB Get and Update on posts, Get on accounts; S3 Put on `socials/*/distribution/published/*` |

Add all three to the `lambda_functions` alarm map in `cloudwatch.tf`. Merge the
Terraform first; CI can deploy code only to a function that already exists.

## Lambdas (saaga-internal-lambdas)

Repo conventions apply: services per concern, `fetchImpl` injected,
`aws-sdk-client-mock`, fixtures, 85% coverage, sops `.env.dev.enc`.

- **`lambda-social-enqueue`**: validate the outbox (schema version, a known
  category, an account row for each platform, media reachable by `HEAD` and not
  expiring before the slot); fan out over accounts; sort by `order`; assign
  slots from the cadence, respecting `not_before`, the order within a video and
  the per-account gap; make conditional puts; cancel rows that an older version
  left unsent. Services: `outbox`, `cadence`, `slots` (a pure function, where
  most of the tests go), `posts`, `accounts`.
- **`lambda-social-calendar`**: for each stream record whose `at`, `status` or
  `rev` changed, upsert schedule `post-<post_id>` (`at()` in UTC, target the
  category queue, input `{post_id, rev}`, `ActionAfterCompletion: DELETE`) when
  the status is `scheduled`, and delete it otherwise. Then rewrite the
  `calendars/YYYY-MM.json` of every month it touched.
- **`lambda-social-publish`**: re-read the row; check `rev`; conditional update
  to `sending`; resolve the account; send through the adapter; write the
  result. A retryable error (5xx, a rate limit, a timeout) returns the row to
  `scheduled` and throws, so SQS retries and then sends it to the DLQ. A
  permanent error (dead account, rejected media, bad copy) sets `failed` with
  the reason and does not retry. Adapters:
  `buffer.service.mjs`, a port of `createPost` from `src/schedule/buffer.rs`
  with `shareNow`, and `pipedream.service.mjs`, which uses `@pipedream/sdk`
  for the proxy or a fresh access token, with one small file per platform for
  its request shape.
- **Later, `lambda-social-health`** (`rate(1 day)`): Pipedream accounts that
  are not `healthy`, disconnected Buffer channels, and the next 48 hours of
  posts whose media is unreachable, posted to Slack.

Secrets in the publisher's sops env: `BUFFER_API_KEY`, `BUFFER_ORG_ID`,
`PIPEDREAM_CLIENT_ID`, `PIPEDREAM_CLIENT_SECRET`, `PIPEDREAM_PROJECT_ID`,
`PIPEDREAM_ENVIRONMENT`.

**Connecting an account** needs no webhook endpoint for a handful of accounts.
A script (later a button in the app) creates a Connect token, opens Pipedream's
hosted connect page, then lists the accounts and writes the
`social-accounts` row.

## App (saaga-social-flow)

1. **Send to calendar.** Queue writes the outbox to the internal bucket
   instead of calling Buffer. `schedule.jsonl` records the outbox key and the
   post ids, which are deterministic, so they are known locally. The Buffer
   queue path stays behind a setting until the scheduler has run for a few
   weeks.
2. **Build Plan names platforms, not channels.** Channel resolution and the
   LinkedIn fan-out move to the accounts table. A skip reason that depends on
   the live channel (disconnected, paused) becomes the enqueue's `held`
   reason.
3. **Analytics** keeps working while Buffer delivers, because the row's
   `result` carries the Buffer post id. Insights for posts sent through
   Pipedream need their own plan.
4. **Calendar tab** (later): a month grid across categories from the table;
   drag to reschedule (`UpdateItem` of `at` and `rev`; the stream does the
   rest); hold, cancel and retry; edit `cadence.json`. It uses the same dev
   SSO login.

## Phases

0. **Spike, with no infrastructure.** In a Pipedream development project,
   connect the LinkedIn page, X, Instagram business, TikTok, the Facebook page
   and YouTube with Pipedream's own clients. For each platform, record
   whether the posting scope is present; whether a text post goes through the
   proxy; how a video gets there (a URL the platform pulls, as Instagram,
   TikTok and Facebook can, against bytes we upload, as LinkedIn, X and YouTube
   need) and whether the proxy can carry the upload; and whether we need our
   own client. Use private or test targets where the platform has them. Price
   Connect production. The result is the adapter table.
1. **Terraform:** the archive prerequisites, `social-scheduler.tf`, and the
   three functions with dummy zips.
2. **Scheduler on Buffer:** enqueue, calendar and publish with the Buffer
   adapter only; `cadence.json`; accounts rows for today's Buffer channels;
   the app writes outboxes. Run one category for real while the others stay on
   the Buffer queue.
3. **Every category on the scheduler.** Remove the app's Buffer queue path.
4. **Pipedream, platform by platform**, in the order the spike recommends.
   Each switch is a change to an accounts row.
5. **Health check** to Slack. The DLQ alarms already exist from phase 1.
6. **Calendar tab** in the app.
7. **Analytics off Buffer** (its own plan), then cancel Buffer.
8. **YouTube longform on the calendar** through `publishAt`, if wanted.

## Open decisions

1. **Shared or per-category accounts?** Does each category post to the same
   LinkedIn page and X account, or do some categories get their own? The
   tables handle both. Shared accounts make the per-account gap matter.
2. **Token ownership.** Recommended: Pipedream holds every token and we hold
   account ids. The alternative is our own client per platform, which means
   the app reviews, with fresh access tokens read at send time. Owning refresh
   tokens outright means dropping Pipedream and running OAuth ourselves.
3. **Cost.** The Pipedream Connect production plan against the Buffer plan it
   would replace, and X API write access, which is paid.
4. **The cadence itself** per category: which platforms, how many a week, what
   times. The categories still depend on the alignment with Laura that the
   archive plan is waiting on.
5. **Timezone** for slots. The draft assumes `America/Los_Angeles`.
6. **Media horizon.** Hold posts past the 30-day expiry (phase 2), or copy the
   finals just in time from the archive (once archive sync exists).
