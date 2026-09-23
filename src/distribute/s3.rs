//! Upload renders to the team's public S3 bucket.
//!
//! A port of `screencast.platforms.media_host` and its `upload_cli`, which this
//! stage used to run through `uv` in a sibling `screencast` checkout — the last
//! piece of the app living in another repo, and what broke when this one moved
//! out from beside it. Key layout and object tags are kept exactly: objects
//! already in the bucket keep their URLs, and the tag vocabulary is a contract
//! with the bucket's lifecycle rules and anything reading `GetObjectTagging`.
//!
//! ## Configuration
//!
//! All from the environment, which `.env` and `dev.sops.env` fill at launch:
//!
//! | variable | meaning |
//! |---|---|
//! | `S3_BUCKET` | the bucket; required |
//! | `S3_REGION` | default `us-east-1` |
//! | `S3_PREFIX` | key prefix under the bucket; default `screencast` |
//! | `S3_PUBLIC_BASE_URL` | a CloudFront or custom domain in front of the bucket |
//! | `S3_ENDPOINT_URL` | only for a non-AWS store such as Cloudflare R2 |
//! | `S3_PUBLIC_ACL` | `true` only on a bucket that still honours ACLs; the team bucket grants public read by policy on the prefix |
//!
//! Credentials come from the standard AWS chain — `AWS_PROFILE=dev` in `.env`
//! and an `aws sso login --profile dev` — the same way `sops` finds them.
//!
//! ## Idempotent by key
//!
//! Video keys carry a short content hash, so a re-render lands at a fresh URL:
//! Buffer caches by URL, and a changed video must never be served stale. The
//! companions — transcripts, artwork — go under a plain name and overwrite in
//! place. Either way an object already at the key is reused, not sent again.

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use aws_sdk_s3::error::{DisplayErrorContext, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, ObjectCannedAcl};
use aws_sdk_s3::Client;
use sha2::{Digest, Sha256};

use super::schema::{DistributeLinks, DistributedAsset};
use super::{Asset, AssetKind, Progress};

/// S3 wants every part but the last at 5 MiB or more. 8 MiB keeps a 200 MB
/// longform to about 25 repaints of the bar, and a part in flight small enough
/// that a dropped connection costs seconds rather than the whole file.
const PART_SIZE: u64 = 8 * 1024 * 1024;

/// Below this a bar is noise — the transcripts and the artwork.
const PROGRESS_MIN_BYTES: u64 = 1_000_000;

/// The characters S3 admits in a tag value; anything else becomes `-`.
const TAG_CHARSET: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 +-=._:/@";

/// S3 caps an object at ten tags. A chapter short spends eight here, so the
/// budget has room; [`object_tags`] checks anyway rather than let S3 reject one
/// chapter's PUT in the middle of a distribute.
const MAX_TAGS: usize = 10;

struct Config {
    bucket: String,
    region: String,
    prefix: String,
    public_base: Option<String>,
    endpoint: Option<String>,
    public_acl: bool,
}

impl Config {
    fn from_env() -> Result<Self> {
        let bucket = non_empty("S3_BUCKET").ok_or_else(|| {
            anyhow!(crate::settings::sops::unset_hint("S3_BUCKET")
                .unwrap_or_else(|| "S3_BUCKET is unset — set it in Settings".to_string()))
        })?;
        Ok(Self {
            bucket,
            region: non_empty("S3_REGION")
                .or_else(|| non_empty("AWS_REGION"))
                .unwrap_or_else(|| "us-east-1".to_string()),
            prefix: non_empty("S3_PREFIX")
                .map(|p| p.trim_matches('/').to_string())
                .unwrap_or_else(|| "screencast".to_string()),
            public_base: non_empty("S3_PUBLIC_BASE_URL")
                .map(|b| b.trim_end_matches('/').to_string()),
            endpoint: non_empty("S3_ENDPOINT_URL"),
            public_acl: non_empty("S3_PUBLIC_ACL")
                .is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes")),
        })
    }

    /// The URL a key resolves to, percent-encoded so a name with a space stays
    /// clickable. `/` is kept: it is the path.
    fn public_url(&self, key: &str) -> String {
        let path = percent_encode(key.trim_start_matches('/'), true);
        match &self.public_base {
            Some(base) => format!("{base}/{path}"),
            None => format!(
                "https://{}.s3.{}.amazonaws.com/{path}",
                self.bucket, self.region
            ),
        }
    }
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Everything one object needs besides its bytes.
struct Put<'a> {
    key: &'a str,
    content_type: &'a str,
    tagging: String,
    acl: Option<ObjectCannedAcl>,
    /// The file name, for the bar.
    label: String,
}

pub fn upload_assets(
    assets: &[Asset],
    project: &str,
    version: u32,
    dest: &Path,
    status: &dyn Fn(&str),
    progress: &dyn Fn(&str, Progress),
) -> Result<DistributeLinks> {
    let config = Config::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting tokio for the S3 upload")?;
    let items = runtime.block_on(async {
        let client = client(&config).await;
        let mut items = Vec::new();
        for (index, asset) in assets.iter().enumerate() {
            status(&format!(
                "Uploading {} ({}/{})…",
                asset.id,
                index + 1,
                assets.len()
            ));
            let key = object_key(&config.prefix, asset, project, version)?;
            let tags = object_tags(asset, project, version)?;
            let url = upload_public(&client, &config, asset, &key, &tags, &|update| {
                progress(&asset.id, update)
            })
            .await?;
            items.push(DistributedAsset {
                id: asset.id.clone(),
                kind: match asset.kind {
                    AssetKind::Long | AssetKind::Chapter => "video".into(),
                    AssetKind::File => "transcript".into(),
                    AssetKind::Image => "image".into(),
                },
                orientation: asset.orientation.clone(),
                chapter: asset.chapter,
                url,
                file: asset
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned()),
            });
        }
        Ok::<_, anyhow::Error>(items)
    })?;
    let links = DistributeLinks {
        project: project.to_string(),
        version,
        items,
    };
    super::schema::save(dest, &links)?;
    Ok(links)
}

async fn client(config: &Config) -> Client {
    let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.region.clone()))
        .profile_name(crate::settings::sops::aws_profile())
        .load()
        .await;
    let mut builder = aws_sdk_s3::config::Builder::from(&shared);
    if let Some(endpoint) = &config.endpoint {
        // Path-style, because a custom endpoint is not guaranteed to resolve
        // `bucket.host` the way AWS does.
        builder = builder.endpoint_url(endpoint).force_path_style(true);
    }
    Client::from_conf(builder.build())
}

async fn upload_public(
    client: &Client,
    config: &Config,
    asset: &Asset,
    key: &str,
    tags: &[(&str, String)],
    on_progress: &dyn Fn(Progress),
) -> Result<String> {
    let url = config.public_url(key);
    let size = std::fs::metadata(&asset.path)
        .with_context(|| format!("reading {}", asset.path.display()))?
        .len();
    let size_mb = size as f64 / (1024.0 * 1024.0);
    if exists(client, &config.bucket, key).await? {
        eprintln!("stream-recorder: already hosted, reusing {url} ({size_mb:.1} MB)");
        return Ok(url);
    }
    let put = Put {
        key,
        content_type: asset.content_type,
        tagging: tag_query(tags),
        acl: config.public_acl.then_some(ObjectCannedAcl::PublicRead),
        label: asset
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| asset.id.clone()),
    };
    eprintln!(
        "stream-recorder: uploading {} ({size_mb:.1} MB) → {url}",
        put.label
    );
    if size > PART_SIZE {
        multipart(client, config, &asset.path, &put, size, on_progress).await?;
    } else {
        let body = ByteStream::from_path(&asset.path)
            .await
            .with_context(|| format!("reading {}", asset.path.display()))?;
        client
            .put_object()
            .bucket(&config.bucket)
            .key(key)
            .body(body)
            .content_type(put.content_type)
            .tagging(&put.tagging)
            .set_acl(put.acl.clone())
            .send()
            .await
            .map_err(|err| explain(&format!("upload failed for {key}"), &err))?;
        if size >= PROGRESS_MIN_BYTES {
            on_progress(Progress::at(size, size, &put.label));
        }
    }
    Ok(url)
}

async fn exists(client: &Client, bucket: &str, key: &str) -> Result<bool> {
    match client.head_object().bucket(bucket).key(key).send().await {
        Ok(_) => Ok(true),
        Err(err) if err.as_service_error().is_some_and(|e| e.is_not_found()) => Ok(false),
        Err(err) => Err(explain(&format!("checking for {key}"), &err)),
    }
}

async fn multipart(
    client: &Client,
    config: &Config,
    path: &Path,
    put: &Put<'_>,
    size: u64,
    on_progress: &dyn Fn(Progress),
) -> Result<()> {
    let started = client
        .create_multipart_upload()
        .bucket(&config.bucket)
        .key(put.key)
        .content_type(put.content_type)
        .tagging(&put.tagging)
        .set_acl(put.acl.clone())
        .send()
        .await
        .map_err(|err| explain(&format!("starting the upload of {}", put.key), &err))?;
    let upload_id = started
        .upload_id()
        .context("S3 started the upload without an id")?
        .to_string();
    match upload_parts(client, config, path, put, &upload_id, size, on_progress).await {
        Ok(parts) => {
            client
                .complete_multipart_upload()
                .bucket(&config.bucket)
                .key(put.key)
                .upload_id(&upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(parts))
                        .build(),
                )
                .send()
                .await
                .map_err(|err| explain(&format!("finishing the upload of {}", put.key), &err))?;
            Ok(())
        }
        Err(err) => {
            // Orphaned parts are billed until aborted; the first error is the
            // one worth reporting, so the abort's own is dropped.
            let _ = client
                .abort_multipart_upload()
                .bucket(&config.bucket)
                .key(put.key)
                .upload_id(&upload_id)
                .send()
                .await;
            Err(err)
        }
    }
}

async fn upload_parts(
    client: &Client,
    config: &Config,
    path: &Path,
    put: &Put<'_>,
    upload_id: &str,
    size: u64,
    on_progress: &dyn Fn(Progress),
) -> Result<Vec<CompletedPart>> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut parts = Vec::new();
    let mut sent = 0u64;
    for number in 1i32.. {
        let mut buf = vec![0u8; PART_SIZE as usize];
        let n = read_up_to(&mut file, &mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        let part = client
            .upload_part()
            .bucket(&config.bucket)
            .key(put.key)
            .upload_id(upload_id)
            .part_number(number)
            .body(ByteStream::from(buf))
            .send()
            .await
            .map_err(|err| explain(&format!("part {number} of {}", put.label), &err))?;
        parts.push(
            CompletedPart::builder()
                .set_e_tag(part.e_tag().map(str::to_string))
                .part_number(number)
                .build(),
        );
        sent += n as u64;
        on_progress(Progress::at(sent, size, &put.label));
        if (n as u64) < PART_SIZE {
            break;
        }
    }
    Ok(parts)
}

/// `read` may return short; a part has to be full or final.
fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// The SDK's `Display` stops at "service error"; the cause is down the source
/// chain, which `DisplayErrorContext` walks. An expired login is the failure a
/// person actually hits, so that one names the command that fixes it.
fn explain<E>(what: &str, err: &SdkError<E>) -> anyhow::Error
where
    E: std::error::Error + 'static,
{
    let detail = format!("{}", DisplayErrorContext(err));
    let lower = detail.to_lowercase();
    let login = ["sso", "token", "credential", "expired"]
        .iter()
        .any(|needle| lower.contains(needle));
    if login {
        let profile = crate::settings::sops::aws_profile();
        anyhow!(
            "{what}: {detail} — run `aws sso login --profile {profile}` and press Distribute again"
        )
    } else {
        anyhow!("{what}: {detail}")
    }
}

fn object_key(prefix: &str, asset: &Asset, project: &str, version: u32) -> Result<String> {
    let base = format!("{prefix}/{project}/v{version}");
    let name = asset
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .with_context(|| format!("{} has no file name", asset.path.display()))?;
    Ok(match asset.kind {
        AssetKind::Long => {
            let orientation = asset.orientation.as_deref().unwrap_or("landscape");
            format!("{base}/{orientation}-{}.mp4", content_digest(&asset.path)?)
        }
        AssetKind::Chapter => {
            let chapter = asset
                .chapter
                .with_context(|| format!("{} is a chapter with no number", asset.id))?;
            let stem = asset
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "chapter".to_string());
            format!(
                "{base}/chapters/{chapter:02}/{stem}-{}.mp4",
                content_digest(&asset.path)?
            )
        }
        AssetKind::File | AssetKind::Image => match asset.chapter {
            Some(chapter) => format!("{base}/chapters/{chapter:02}/{name}"),
            None => format!("{base}/{name}"),
        },
    })
}

/// The first eight hex digits of the file's SHA-256 — enough that a re-render
/// with different bytes gets a different key, which is all it is for.
fn content_digest(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Tags for one object — a fixed vocabulary, not free text: the bucket's
/// lifecycle rules and anything reading `GetObjectTagging` match on these
/// exact values. `retain` says which objects may expire: video is the cost
/// and only has to outlive the platform's fetch; transcripts and artwork are
/// kilobytes and the part worth keeping.
///
/// The Python uploader knew only `video` and `transcript` and filed artwork
/// under the latter; it is `image` here, because a consumer filtering on
/// `asset` should see what the object is.
fn object_tags(asset: &Asset, project: &str, version: u32) -> Result<Vec<(&'static str, String)>> {
    let (what, retain) = match asset.kind {
        AssetKind::Long | AssetKind::Chapter => ("video", "ephemeral"),
        AssetKind::File => ("transcript", "durable"),
        AssetKind::Image => ("image", "durable"),
    };
    let mut tags = vec![
        ("app", "screencast".to_string()),
        ("asset", what.to_string()),
        ("project", tag_safe(project)),
        ("version", version.to_string()),
        ("retain", retain.to_string()),
    ];
    match asset.kind {
        AssetKind::Long => tags.push(("form", "long".to_string())),
        AssetKind::Chapter => tags.push(("form", "short".to_string())),
        AssetKind::File | AssetKind::Image => {}
    }
    // Meaningless for a transcript, so omitted rather than filled with a
    // placeholder a rule could match by accident.
    if asset.kind != AssetKind::File {
        if let Some(orientation) = &asset.orientation {
            tags.push(("orientation", tag_safe(orientation)));
        }
    }
    if let Some(chapter) = asset.chapter {
        tags.push(("chapter", format!("{chapter:02}")));
    }
    if tags.len() > MAX_TAGS {
        bail!(
            "{} tags exceeds S3's limit of {MAX_TAGS} on {}",
            tags.len(),
            asset.id
        );
    }
    Ok(tags)
}

fn tag_safe(value: &str) -> String {
    let safe: String = value
        .chars()
        .map(|c| if TAG_CHARSET.contains(c) { c } else { '-' })
        .collect();
    match safe.trim() {
        "" => "unknown".to_string(),
        trimmed => trimmed.to_string(),
    }
}

/// The wire form of the tags: a query string, sent with the upload itself. A
/// follow-up `PutObjectTagging` would fire a second event that consumers race
/// against the first.
fn tag_query(tags: &[(&str, String)]) -> String {
    tags.iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k, false), percent_encode(v, false)))
        .collect::<Vec<_>>()
        .join("&")
}

/// RFC 3986 unreserved characters pass; everything else is `%XX`. `keep_slash`
/// is for a key used as a URL path.
fn percent_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let plain = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/');
        if plain {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_file(tag: &str, bytes: &[u8]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stream-recorder-s3-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{tag}.mp4"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn asset(
        kind: AssetKind,
        path: PathBuf,
        orientation: Option<&str>,
        chapter: Option<u32>,
    ) -> Asset {
        Asset {
            id: "x".into(),
            kind,
            path,
            content_type: "video/mp4",
            orientation: orientation.map(str::to_string),
            chapter,
        }
    }

    fn config() -> Config {
        Config {
            bucket: "media".into(),
            region: "us-east-1".into(),
            prefix: "screencast".into(),
            public_base: None,
            endpoint: None,
            public_acl: false,
        }
    }

    /// The layout the Python uploader wrote, byte for byte: videos get a hash
    /// so a re-render is a new URL, companions overwrite in place.
    #[test]
    fn video_keys_carry_a_content_hash_and_companions_do_not() {
        let long = temp_file("long", b"the longform");
        let key = object_key(
            "screencast",
            &asset(AssetKind::Long, long, Some("landscape"), None),
            "vd-1",
            2,
        )
        .unwrap();
        assert!(key.starts_with("screencast/vd-1/v2/landscape-"), "{key}");
        assert!(key.ends_with(".mp4"), "{key}");
        let digest = key
            .trim_start_matches("screencast/vd-1/v2/landscape-")
            .trim_end_matches(".mp4");
        assert_eq!(digest.len(), 8, "{key}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{key}");

        let chapter = temp_file("chapter-03", b"a short");
        let key = object_key(
            "screencast",
            &asset(AssetKind::Chapter, chapter, Some("portrait"), Some(3)),
            "vd-1",
            2,
        )
        .unwrap();
        assert!(
            key.starts_with("screencast/vd-1/v2/chapters/03/chapter-03-"),
            "{key}"
        );

        let transcript = asset(
            AssetKind::File,
            PathBuf::from("/tmp/transcript.txt"),
            None,
            None,
        );
        assert_eq!(
            object_key("screencast", &transcript, "vd-1", 2).unwrap(),
            "screencast/vd-1/v2/transcript.txt"
        );
        let chapter_txt = asset(
            AssetKind::File,
            PathBuf::from("/tmp/chapter-03.txt"),
            None,
            Some(3),
        );
        assert_eq!(
            object_key("screencast", &chapter_txt, "vd-1", 2).unwrap(),
            "screencast/vd-1/v2/chapters/03/chapter-03.txt"
        );
    }

    #[test]
    fn a_rerender_with_different_bytes_gets_a_different_key() {
        let one = temp_file("take-one", b"first cut");
        let two = temp_file("take-two", b"second cut");
        let key = |path| {
            object_key(
                "screencast",
                &asset(AssetKind::Long, path, None, None),
                "p",
                1,
            )
            .unwrap()
        };
        assert_ne!(key(one), key(two));
    }

    /// The fixed vocabulary: two consumers match on these exact values.
    #[test]
    fn tags_follow_the_fixed_vocabulary() {
        let long = asset(
            AssetKind::Long,
            PathBuf::from("/tmp/longform.mp4"),
            Some("landscape"),
            None,
        );
        let tags = object_tags(&long, "vd 1", 3).unwrap();
        let get = |k: &str| {
            tags.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("app"), Some("screencast"));
        assert_eq!(get("asset"), Some("video"));
        assert_eq!(get("project"), Some("vd 1"));
        assert_eq!(get("version"), Some("3"));
        assert_eq!(get("retain"), Some("ephemeral"));
        assert_eq!(get("form"), Some("long"));
        assert_eq!(get("orientation"), Some("landscape"));
        assert_eq!(get("chapter"), None);

        let short = asset(
            AssetKind::Chapter,
            PathBuf::from("/tmp/chapter-03.mp4"),
            Some("portrait"),
            Some(3),
        );
        let tags = object_tags(&short, "vd-1", 1).unwrap();
        let get = |k: &str| {
            tags.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("form"), Some("short"));
        assert_eq!(get("chapter"), Some("03"), "zero-padded, like the key");
        assert!(tags.len() <= MAX_TAGS);

        // A transcript has no orientation even when the caller passes one.
        let transcript = asset(
            AssetKind::File,
            PathBuf::from("/tmp/t.txt"),
            Some("landscape"),
            None,
        );
        let tags = object_tags(&transcript, "vd-1", 1).unwrap();
        let get = |k: &str| {
            tags.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("asset"), Some("transcript"));
        assert_eq!(get("retain"), Some("durable"));
        assert_eq!(get("orientation"), None);
        assert_eq!(get("form"), None);

        let image = asset(
            AssetKind::Image,
            PathBuf::from("/tmp/cover.jpg"),
            Some("landscape"),
            None,
        );
        let tags = object_tags(&image, "vd-1", 1).unwrap();
        let get = |k: &str| {
            tags.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("asset"), Some("image"));
        assert_eq!(get("retain"), Some("durable"));
        assert_eq!(get("orientation"), Some("landscape"));
    }

    #[test]
    fn tag_values_are_coerced_into_the_charset() {
        assert_eq!(tag_safe("vd/1 ok:v@2+3=4"), "vd/1 ok:v@2+3=4");
        assert_eq!(tag_safe("vd#1 (final)"), "vd-1 -final-");
        assert_eq!(tag_safe("   "), "unknown");
        assert_eq!(tag_safe(" trimmed "), "trimmed");
    }

    /// `x-amz-tagging` is a query string, so the values it carries have to be
    /// encoded like one — a project name with a space or a colon included.
    #[test]
    fn the_tag_query_is_url_encoded() {
        let tags = vec![
            ("project", "a b:c".to_string()),
            ("version", "2".to_string()),
        ];
        assert_eq!(tag_query(&tags), "project=a%20b%3Ac&version=2");
    }

    #[test]
    fn public_urls_come_from_the_base_or_the_bucket() {
        let plain = config();
        assert_eq!(
            plain.public_url("screencast/p/v1/landscape-abc12345.mp4"),
            "https://media.s3.us-east-1.amazonaws.com/screencast/p/v1/landscape-abc12345.mp4"
        );
        assert_eq!(
            plain.public_url("screencast/p/v1/my chapter.mp4"),
            "https://media.s3.us-east-1.amazonaws.com/screencast/p/v1/my%20chapter.mp4",
            "a space is encoded, the slashes are kept"
        );
        let fronted = Config {
            public_base: Some("https://media.saaga.dev".into()),
            ..config()
        };
        assert_eq!(
            fronted.public_url("screencast/p/v1/t.txt"),
            "https://media.saaga.dev/screencast/p/v1/t.txt"
        );
    }

    #[test]
    fn progress_reports_bytes_as_megabytes_and_a_clamped_percent() {
        let half = Progress::at(5 * 1024 * 1024, 10 * 1024 * 1024, "a.mp4");
        assert_eq!(half.pct, 50.0);
        assert_eq!(half.seen_mb, 5.0);
        assert_eq!(half.total_mb, 10.0);
        assert_eq!(half.label, "a.mp4");
        let over = Progress::at(11, 10, "a.mp4");
        assert_eq!(over.pct, 100.0);
        assert_eq!(over.seen_mb, over.total_mb, "seen never exceeds total");
    }
}
