//! Buffer GraphQL transport — list channels, queue a post. Nothing decides *what*
//! to post here; the planner does that and hands this module a finished input.
//!
//! Every document here was introspected against the live api: enum values are
//! lowercase (`addToQueue`, `reel`, `public`), a video asset is
//! `{"video": {"url": ...}}` (no `source`, no `type`), and `createPost` hands the
//! new post id back directly — matching a post by text afterwards is broken by
//! design, because `Channel.linkShortening` rewrites text server-side.
//! Per-service `metadata` shapes live in [`super::meta`].

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const ENDPOINT: &str = "https://api.buffer.com";

const ACCOUNT_QUERY: &str = "query Account { account { organizations { id name } } }";

const CHANNELS_QUERY: &str = r#"query Channels($input: ChannelsInput!) {
  channels(input: $input) {
    id name displayName service type timezone isDisconnected isQueuePaused
    postingSchedule { day times paused }
    metadata {
      ... on InstagramMetadata { defaultToReminders }
      ... on TiktokMetadata { defaultToReminders }
      ... on YoutubeMetadata { defaultToReminders }
    }
  }
}"#;

const CREATE_POST_MUTATION: &str = r#"mutation CreatePost($input: CreatePostInput!) {
  createPost(input: $input) {
    __typename
    ... on PostActionSuccess { post { id dueAt status } }
    ... on MutationError { message }
    ... on LimitReachedError { message }
  }
}"#;

const DELETE_POST_MUTATION: &str = r#"mutation DeletePost($input: DeletePostInput!) {
  deletePost(input: $input) {
    __typename
    ... on DeletePostSuccess { id }
    ... on MutationError { message }
  }
}"#;

/// Everything still waiting in the queue. `sent` is excluded deliberately — a
/// published post is not queued, and deleting its record would not unpublish it.
const PENDING_POSTS_QUERY: &str = r#"query Posts($input: PostsInput!, $first: Int, $after: String) {
  posts(input: $input, first: $first, after: $after) {
    edges { node { id status channelService } }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// Statuses that count as "in the queue" for a clear — deletable, not yet out.
pub const PENDING_STATUSES: [&str; 3] = ["scheduled", "needs_approval", "error"];

/// Every status a post can hold. Used to tell "already deleted" apart from
/// "already published", which are opposite answers to the same question.
pub const ALL_STATUSES: [&str; 6] = [
    "draft",
    "needs_approval",
    "scheduled",
    "sending",
    "sent",
    "error",
];

pub struct BufferClient {
    api_key: String,
    org_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleSlot {
    pub day: String,
    pub times: Vec<String>,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub service: String,
    pub kind: String,
    pub timezone: String,
    pub is_disconnected: bool,
    pub is_queue_paused: bool,
    pub default_to_reminders: Option<bool>,
    pub posting_schedule: Vec<ScheduleSlot>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatePostInput {
    pub channel_id: String,
    pub text: String,
    pub mode: String,
    pub scheduling_type: String,
    pub needs_approval: bool,
    pub image: bool,
    pub video_url: String,
    pub video_title: Option<String>,
    pub ai_assisted: bool,
    /// The `PostInputMetaData` object the plan already showed, sent verbatim.
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedPost {
    pub id: String,
    pub due_at: Option<String>,
    pub status: String,
}

impl BufferClient {
    pub fn from_env() -> Result<Self> {
        let api_key = env_value("BUFFER_API_KEY")
            .context("BUFFER_API_KEY is not set — add it to the project .env")?;
        let org_id = match env_value("BUFFER_ORG_ID") {
            Some(id) => id,
            None => {
                let id = resolve_org_id(&api_key)?;
                eprintln!("buffer: BUFFER_ORG_ID unset, resolved organization {id}");
                id
            }
        };
        Ok(Self { api_key, org_id })
    }

    /// The token this client authenticates with — for sibling modules that issue
    /// their own queries through [`graphql`].
    pub(super) fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn channels(&self) -> Result<Vec<Channel>> {
        let vars = json!({ "input": { "organizationId": self.org_id } });
        let data = graphql(&self.api_key, CHANNELS_QUERY, vars)?;
        let items = data.get("channels").and_then(Value::as_array);
        let items = items.context("buffer channels response carried no channels array")?;
        Ok(items.iter().map(parse_channel).collect())
    }

    #[tracing::instrument(skip(self, input), fields(channel = %input.channel_id))]
    pub fn create_post(&self, input: &CreatePostInput) -> Result<CreatedPost> {
        let data = graphql(
            &self.api_key,
            CREATE_POST_MUTATION,
            create_post_variables(input),
        )?;
        let payload = data.get("createPost").filter(|value| !value.is_null());
        parse_created_post(payload.context("buffer createPost carried no payload")?)
    }

    /// Removes one post from the queue. Irreversible at Buffer's end.
    #[tracing::instrument(skip(self))]
    pub fn delete_post(&self, id: &str) -> Result<()> {
        let vars = json!({ "input": { "id": id } });
        let data = graphql(&self.api_key, DELETE_POST_MUTATION, vars)?;
        let payload = data
            .get("deletePost")
            .filter(|value| !value.is_null())
            .context("buffer deletePost carried no payload")?;
        // The union collapses to a `message` only on the error arm, so a message
        // present is the failure — same shape the create path reads.
        if let Some(message) = opt_string_at(payload, "message") {
            bail!("buffer deletePost failed: {message}");
        }
        Ok(())
    }

    /// Every post still waiting to go out, across every channel in the org.
    pub fn pending_posts(&self) -> Result<Vec<PendingPost>> {
        self.posts_by_status(&PENDING_STATUSES)
    }

    /// Posts in any of `statuses`, across every channel in the org.
    ///
    /// Paged to exhaustion rather than capped: a clear that silently stopped at
    /// the first page would report success over a queue it only half emptied.
    #[tracing::instrument(skip(self))]
    pub fn posts_by_status(&self, statuses: &[&str]) -> Result<Vec<PendingPost>> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let vars = json!({
                "input": {
                    "organizationId": self.org_id,
                    "filter": { "status": statuses },
                },
                "first": 100,
                "after": after,
            });
            let data = graphql(&self.api_key, PENDING_POSTS_QUERY, vars)?;
            let posts = data
                .get("posts")
                .context("buffer posts carried no results")?;
            for edge in array_at(posts, "edges") {
                let Some(node) = edge.get("node") else {
                    continue;
                };
                let Some(id) = opt_string_at(node, "id") else {
                    continue;
                };
                out.push(PendingPost {
                    id,
                    status: opt_string_at(node, "status").unwrap_or_default(),
                    service: opt_string_at(node, "channelService").unwrap_or_default(),
                });
            }
            let page = posts.get("pageInfo");
            let more = page
                .and_then(|info| info.get("hasNextPage"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            after = page.and_then(|info| opt_string_at(info, "endCursor"));
            if !more || after.is_none() {
                break;
            }
        }
        Ok(out)
    }
}

/// One post sitting in the Buffer queue, as a clear needs to see it.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPost {
    pub id: String,
    pub status: String,
    pub service: String,
}

/// The `{"input": {...}}` variables for `CreatePost` — pure so tests can assert it.
pub fn create_post_variables(input: &CreatePostInput) -> Value {
    let mut object = json!({
        "channelId": input.channel_id,
        "text": input.text,
        "mode": trimmed(Some(input.mode.as_str())).unwrap_or("addToQueue"),
        "schedulingType": trimmed(Some(input.scheduling_type.as_str())).unwrap_or("automatic"),
        "needsApproval": input.needs_approval,
        "aiAssisted": input.ai_assisted,
        "assets": if input.image { json!([{ "image": { "url": input.video_url } }]) } else { video_assets(&input.video_url, input.video_title.as_deref()) },
    });
    if let Some(meta) = input.metadata.clone() {
        object["metadata"] = meta;
    }
    json!({ "input": object })
}

/// `AssetInput` is @oneOf: video carries a bare `url` plus optional `metadata.title`.
///
/// Notably *not* `thumbnailUrl`. The field exists on `VideoAssetInput`, so the
/// schema accepts it and only the server refuses, with: "Video thumbnailUrl is
/// not supported: social networks do not accept custom video thumbnail images,
/// so this value is never sent to the network." Sending it fails the whole post,
/// so a generated thumbnail cannot reach a network through Buffer at all.
fn video_assets(url: &str, title: Option<&str>) -> Value {
    let Some(url) = trimmed(Some(url)) else {
        return json!([]);
    };
    let mut video = json!({ "url": url });
    if let Some(title) = trimmed(title) {
        video["metadata"] = json!({ "title": title });
    }
    json!([{ "video": video }])
}

pub(super) fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn env_value(key: &str) -> Option<String> {
    let value = std::env::var(key).ok()?;
    trimmed(Some(value.as_str())).map(str::to_string)
}

fn resolve_org_id(api_key: &str) -> Result<String> {
    let data = graphql(api_key, ACCOUNT_QUERY, Value::Null)?;
    let orgs = organizations(&data);
    // Picking arbitrarily from several organizations posts to the wrong workspace,
    // and the symptom ("no instagram channel connected") points nowhere near the
    // cause — so name every candidate and say how to pin one.
    if orgs.len() > 1 {
        let listed: Vec<String> = orgs
            .iter()
            .map(|(id, name)| format!("{name} ({id})"))
            .collect();
        eprintln!(
            "buffer: account has {} organizations [{}] — using the first; set BUFFER_ORG_ID to choose",
            orgs.len(),
            listed.join(", ")
        );
    }
    let (id, _) = orgs
        .into_iter()
        .next()
        .context("buffer account has no organizations — set BUFFER_ORG_ID")?;
    Ok(id)
}

/// Every organization on the account as `(id, name)`, in api order.
fn organizations(data: &Value) -> Vec<(String, String)> {
    let orgs = data
        .get("account")
        .map(|account| array_at(account, "organizations"));
    orgs.unwrap_or_default()
        .iter()
        .filter_map(|org| {
            let id = opt_string_at(org, "id")?;
            let name = opt_string_at(org, "name").unwrap_or_else(|| "unnamed".to_string());
            Some((id, name))
        })
        .collect()
}

/// A 200 carrying a non-empty top-level `errors` array is still a failure.
fn graphql_errors(body: &Value) -> Option<String> {
    let errors = body
        .get("errors")?
        .as_array()
        .filter(|list| !list.is_empty())?;
    let messages: Vec<String> = errors
        .iter()
        .map(|error| opt_string_at(error, "message").unwrap_or_else(|| error.to_string()))
        .collect();
    Some(messages.join("; "))
}

pub(super) fn graphql(api_key: &str, query: &str, variables: Value) -> Result<Value> {
    let mut body = json!({ "query": query });
    if !variables.is_null() {
        body["variables"] = variables;
    }
    let sent = ureq::post(ENDPOINT)
        .timeout(Duration::from_secs(60))
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body);
    let response = match sent {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            bail!("buffer graphql http {code}: {}", text.trim());
        }
        Err(err) => return Err(err).context("calling buffer graphql"),
    };
    let value: Value = response
        .into_json()
        .context("parsing buffer graphql body")?;
    if let Some(message) = graphql_errors(&value) {
        bail!("buffer graphql error: {message}");
    }
    let data = value.get("data").filter(|data| !data.is_null()).cloned();
    let data = data.context("buffer graphql response carried no data")?;
    inspect_data_errors(&data)?;
    Ok(data)
}

/// Like SAAGA's buffer-graphql client, HTTP 200 can still contain an error union.
fn inspect_data_errors(data: &Value) -> Result<()> {
    let Some(fields) = data.as_object() else {
        return Ok(());
    };
    for node in fields.values() {
        let kind = node
            .get("__typename")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(
            kind,
            "MutationError"
                | "NotFoundError"
                | "LimitReachedError"
                | "AuthorizationError"
                | "ValidationError"
                | "PostPublishingError"
        ) {
            let message = node
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("request refused");
            bail!("buffer {kind}: {message}");
        }
    }
    Ok(())
}

fn parse_created_post(payload: &Value) -> Result<CreatedPost> {
    if let Some(post) = payload.get("post").filter(|value| !value.is_null()) {
        return Ok(CreatedPost {
            id: opt_string_at(post, "id")
                .filter(|id| !id.trim().is_empty())
                .context("buffer createPost returned no post id")?,
            due_at: opt_string_at(post, "dueAt"),
            status: string_at(post, "status"),
        });
    }
    if let Some(message) = payload.get("message").and_then(Value::as_str) {
        bail!("buffer createPost failed: {message}");
    }
    bail!("buffer createPost returned an unrecognised payload: {payload}")
}

fn parse_channel(raw: &Value) -> Channel {
    Channel {
        id: string_at(raw, "id"),
        name: string_at(raw, "name"),
        display_name: string_at(raw, "displayName"),
        service: string_at(raw, "service"),
        kind: string_at(raw, "type"),
        timezone: string_at(raw, "timezone"),
        is_disconnected: bool_at(raw, "isDisconnected"),
        is_queue_paused: bool_at(raw, "isQueuePaused"),
        // Only Instagram/Tiktok/Youtube metadata select it; others stay None.
        default_to_reminders: raw
            .get("metadata")
            .and_then(|meta| meta.get("defaultToReminders"))
            .and_then(Value::as_bool),
        posting_schedule: array_at(raw, "postingSchedule")
            .iter()
            .map(|slot| ScheduleSlot {
                day: string_at(slot, "day"),
                times: strings_at(slot, "times"),
                paused: bool_at(slot, "paused"),
            })
            .collect(),
    }
}

fn string_at(raw: &Value, key: &str) -> String {
    opt_string_at(raw, key).unwrap_or_default()
}

fn opt_string_at(raw: &Value, key: &str) -> Option<String> {
    raw.get(key).and_then(Value::as_str).map(str::to_string)
}

fn bool_at(raw: &Value, key: &str) -> bool {
    raw.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn strings_at(raw: &Value, key: &str) -> Vec<String> {
    array_at(raw, key)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn array_at<'a>(raw: &'a Value, key: &str) -> &'a [Value] {
    raw.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(metadata: Option<Value>, video_title: Option<&str>) -> CreatePostInput {
        CreatePostInput {
            channel_id: "6a3dbb795ab6d2f10671b945".into(),
            text: "hello".into(),
            mode: "addToQueue".into(),
            scheduling_type: "notification".into(),
            needs_approval: false,
            image: false,
            video_url: "https://s3.example.com/vertical/chapter-01.mp4".into(),
            video_title: video_title.map(str::to_string),
            ai_assisted: true,
            metadata,
        }
    }

    #[test]
    fn variables_use_lowercase_enums_and_a_url_keyed_video_asset() {
        let meta = json!({ "instagram": { "type": "reel", "shouldShareToFeed": true } });
        let vars = create_post_variables(&sample(Some(meta), Some("Chapter One")));
        let object = &vars["input"];
        assert_eq!(object.get("schedulingType"), Some(&json!("notification")));
        assert_eq!(object["mode"], "addToQueue");
        assert_eq!(object["needsApproval"], false);
        assert_eq!(object["aiAssisted"], true);
        let asset = &object["assets"][0];
        // AssetInput is @oneOf on the media kind: no "source", no "type: VIDEO".
        assert!(asset.get("source").is_none() && asset.get("type").is_none());
        let video = &asset["video"];
        assert_eq!(
            video["url"],
            "https://s3.example.com/vertical/chapter-01.mp4"
        );
        assert!(video.get("source").is_none() && video.get("type").is_none());
        assert_eq!(video["metadata"]["title"], "Chapter One");
        // The planner's metadata rides through untouched.
        assert_eq!(object["metadata"]["instagram"]["type"], "reel");
    }

    /// A custom cover must never be attached. `VideoAssetInput` declares
    /// `thumbnailUrl`, so this passes schema validation and fails at the server:
    /// "Video thumbnailUrl is not supported: social networks do not accept custom
    /// video thumbnail images". It took six rejected posts to learn; this keeps it
    /// learned.
    #[test]
    fn a_video_asset_never_carries_a_custom_thumbnail() {
        let video = create_post_variables(&sample(None, Some("The Long One")))["input"]["assets"]
            [0]["video"]
            .clone();
        assert!(video.get("thumbnailUrl").is_none());
        assert_eq!(video["metadata"], json!({ "title": "The Long One" }));
    }

    #[test]
    fn a_blank_mode_or_scheduling_type_degrades_to_a_valid_enum() {
        let mut input = sample(None, None);
        input.mode = "  ".into();
        input.scheduling_type = String::new();
        let object = create_post_variables(&input)["input"].clone();
        assert_eq!(object["mode"], "addToQueue");
        assert_eq!(object["schedulingType"], "automatic");
    }

    #[test]
    fn an_item_without_metadata_or_a_url_sends_neither() {
        // `Value` indexing yields null for a missing key, so null == key omitted.
        let untitled = create_post_variables(&sample(None, None));
        assert!(untitled["input"]["metadata"].is_null());
        assert!(untitled["input"]["assets"][0]["video"]
            .get("metadata")
            .is_none());
        let mut no_url = sample(None, None);
        no_url.video_url = "  ".into();
        // assets is non-null on the input, so an empty list — never a null.
        assert_eq!(create_post_variables(&no_url)["input"]["assets"], json!([]));
    }

    #[test]
    fn errors_are_detected_in_the_envelope_and_in_the_create_post_payload() {
        let body = json!({ "data": null, "errors": [{ "message": "bad enum" }, { "m": 1 }] });
        assert_eq!(
            graphql_errors(&body).as_deref(),
            Some(r#"bad enum; {"m":1}"#)
        );
        assert!(graphql_errors(&json!({ "data": { "ok": true } })).is_none());
        assert!(graphql_errors(&json!({ "errors": [] })).is_none());
        let post = json!({ "id": "abc", "dueAt": "2026-08-15T17:00:00Z", "status": "buffer" });
        let ok = parse_created_post(&json!({ "post": post })).unwrap();
        assert_eq!(ok.id, "abc");
        assert_eq!(ok.due_at.as_deref(), Some("2026-08-15T17:00:00Z"));
        assert_eq!(ok.status, "buffer");
        let err = parse_created_post(&json!({ "message": "channel is disconnected" })).unwrap_err();
        assert!(err.to_string().contains("channel is disconnected"));
    }

    #[test]
    fn channel_parsing_keeps_reminders_only_when_the_union_carried_it() {
        let channel = parse_channel(&json!({
            "id": "6a3dbb555ab6d2f10671b8cf", "name": "andrewmelnychukos",
            "displayName": "Andrew", "service": "tiktok", "type": "account",
            "timezone": "America/Edmonton", "isDisconnected": false, "isQueuePaused": false,
            "postingSchedule": [{ "day": "mon", "times": ["09:00", "17:00"], "paused": false }],
            "metadata": { "defaultToReminders": true }
        }));
        assert_eq!(channel.kind, "account");
        assert_eq!(channel.default_to_reminders, Some(true));
        assert_eq!(channel.posting_schedule[0].times, vec!["09:00", "17:00"]);
        let bare = parse_channel(&json!({ "id": "x", "service": "twitter", "metadata": {} }));
        assert_eq!(bare.default_to_reminders, None);
        assert!(bare.posting_schedule.is_empty());
    }

    #[test]
    fn organizations_are_listed_with_their_names_so_a_wrong_pick_is_visible() {
        let data = json!({ "account": { "organizations": [
            { "id": "org-1", "name": "SAAGA" }, { "id": "o2" }
        ] } });
        assert_eq!(
            organizations(&data),
            vec![
                ("org-1".to_string(), "SAAGA".to_string()),
                ("o2".to_string(), "unnamed".to_string()),
            ]
        );
        assert!(organizations(&json!({ "account": { "organizations": [] } })).is_empty());
        assert!(organizations(&json!({})).is_empty());
    }
    #[test]
    fn http_success_does_not_hide_buffer_error_unions() {
        for kind in [
            "MutationError",
            "NotFoundError",
            "LimitReachedError",
            "AuthorizationError",
            "ValidationError",
            "PostPublishingError",
        ] {
            let error = inspect_data_errors(
                &json!({"createPost": {"__typename": kind, "message": "Refused"}}),
            )
            .unwrap_err();
            assert!(error.to_string().contains(kind));
            assert!(error.to_string().contains("Refused"));
        }
        assert!(inspect_data_errors(
            &json!({"createPost": {"__typename": "PostActionSuccess", "post": {"id": "1"}}})
        )
        .is_ok());
        assert!(parse_created_post(&json!({"post": {"status": "scheduled"}})).is_err());
    }
}
