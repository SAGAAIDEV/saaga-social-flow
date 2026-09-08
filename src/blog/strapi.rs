//! The Strapi v5 REST client for `video-post`.
//!
//! Ported from `saaga-martech/posts/platforms/strapi.py`, and four of its
//! behaviours are kept deliberately because each one is a bug that was already
//! paid for once:
//!
//! 1. **Relations are verified, not assumed.** Some Strapi v5 and
//!    token-permission combinations accept a bare numeric relation id on `POST`
//!    and silently drop it. The create asks for the relations to be echoed back
//!    and re-sends any that came back null as a `PUT`.
//! 2. **Lookups are case-insensitive and try two fields.** `$eqi` on `name`,
//!    then `slug`, so "Education" matches an entry stored as "education". A
//!    network blip on the first attempt falls through to the second rather than
//!    aborting.
//! 3. **A failed publish is not a failed create.** The entry exists; reporting
//!    it as an error invites a retry that duplicates it.
//!
//! The fourth behaviour was ported and turned out to be wrong. The Python did
//! `POST /api/video-posts` and then `POST /{documentId}/actions/publish`, and
//! that second route does not exist in Strapi 5's REST API — the live CMS
//! answers `405` with `allow: HEAD, GET`, because `/actions/publish` belongs to
//! the admin Content-Manager API. Every post made that way stayed a draft and
//! reported "created as draft (publish failed)".
//!
//! Publishing over REST is `?status=published` on the write itself. In
//! `@strapi/core` 5.27 the document service's `create` ends:
//!
//! ```js
//! if (hasDraftAndPublish && params.status === 'published') {
//!     return publish({ ...params, documentId: doc.documentId })
//! }
//! ```
//!
//! `update` has the identical tail, and both run `setStatusToDraft` first — so a
//! write *without* the parameter touches only the draft. That is why the
//! relation retry below carries the flag too: patching a dropped relation
//! without it would fix the draft and leave the live page still missing it.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::compat;
use super::library::Entry;
use super::payload::{NewVideoPost, RELATIONS};

const UPLOAD_PATH: &str = "/api/upload";
const VIDEO_POSTS_PATH: &str = "/api/video-posts";
const AUTHORS_PATH: &str = "/api/authors";
const CATEGORIES_PATH: &str = "/api/categories";

/// Strapi 5's draft/published selector, on writes as well as reads.
const STATUS: &str = "status";
const PUBLISHED: &str = "published";

/// What the media library did with a file.
///
/// The thumbnail is set as a *relation* and needs only the id; a figure is set
/// as a plain `src` string and needs the URL. Both come back from the same
/// response, and the URL used to be parsed and thrown away.
pub struct Uploaded {
    pub id: i64,
    /// Absolute. `/api/upload` answers with a path like
    /// `/uploads/figure_01_abc.jpg` on a self-hosted install, and a figure
    /// block's `src` is not run through the site's media-URL rewriter — that
    /// only fires for a media relation — so a relative URL here is a 404 on the
    /// live page.
    pub url: String,
}

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Where a video upload stops being worth trying over one HTTP request. Not a
/// Strapi limit — a limit on reading a file into memory twice and holding a
/// connection open for five minutes.
const VIDEO_UPLOAD_MAX: u64 = 512 * 1_048_576;

/// Where a published entry can be read. Overridable because the CMS host and the
/// site host are different machines and only one of them is in `.env`.
fn public_base() -> String {
    std::env::var("BLOG_PUBLIC_BASE")
        .ok()
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://saagasolve.com".to_string())
}

pub struct Strapi {
    base: String,
    token: String,
}

/// What a successful create leaves behind.
#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub document_id: String,
    pub entry_id: i64,
    pub slug: String,
    pub public_url: String,
    pub admin_url: String,
    pub published: bool,
    /// Set when the entry exists but something after it did not — a publish that
    /// failed, or a relation that would not stick. Not an error: the entry is
    /// real, and a retry would duplicate it.
    pub warning: Option<String>,
}

impl Strapi {
    pub fn from_env() -> Result<Strapi> {
        let base = env("STRAPI_API_URL")
            .context("STRAPI_API_URL is unset — add it to stream-recorder/.env")?;
        let token = env("STRAPI_API_TOKEN")
            .context("STRAPI_API_TOKEN is unset — add it to stream-recorder/.env")?;
        Ok(Strapi {
            base: base.trim_end_matches('/').to_string(),
            token,
        })
    }

    /// A client pointed at nowhere, for the tests of callers that must decide
    /// *not* to call. Constructible only in tests, because a client with no
    /// real base is a mistake anywhere else.
    #[cfg(test)]
    pub fn for_test() -> Strapi {
        Strapi {
            base: "http://127.0.0.1:0".into(),
            token: "test".into(),
        }
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    // ------------------------------------------------------------- lookups

    pub fn find_author(&self, name: &str) -> Option<i64> {
        self.find_relation(AUTHORS_PATH, "author", name)
    }

    pub fn find_category(&self, name: &str) -> Option<i64> {
        self.find_relation(CATEGORIES_PATH, "category", name)
    }

    /// Every author in the CMS, for the picker.
    ///
    /// Ordered by name and keyed by id rather than by name, which is not a
    /// detail: the live collection has two rows both called "Danish Rafique".
    /// Matching a byline by name would pick one of them arbitrarily.
    pub fn list_authors(&self) -> Result<Vec<Entry>> {
        self.list(AUTHORS_PATH, "author", "jobTitle")
    }

    /// Every category — the one taxonomy a video post carries, shared with
    /// `blog-articles` and the thing `/blog/category/[name]` reads.
    pub fn list_categories(&self) -> Result<Vec<Entry>> {
        self.list(CATEGORIES_PATH, "category", "description")
    }

    /// One page of 100, which is well past what either collection holds and
    /// keeps this to a single request. A collection that outgrows it truncates
    /// rather than paginating, so it says so.
    fn list(&self, path: &str, label: &str, detail_field: &str) -> Result<Vec<Entry>> {
        let url = format!("{}{path}", self.base);
        let response = ureq::get(&url)
            .set("Authorization", &self.bearer())
            .query("sort", "name:asc")
            .query("pagination[pageSize]", "100")
            .timeout(LOOKUP_TIMEOUT)
            .call();
        let body: serde_json::Value = match response {
            Ok(response) => response
                .into_json()
                .with_context(|| format!("parsing the {label} list"))?,
            Err(ureq::Error::Status(code, response)) => {
                let detail = response.into_string().unwrap_or_default();
                // 401/403 here is the token, and saying so is worth more than the
                // status line: reads are refused outright without a valid one.
                bail!(
                    "strapi refused the {label} list ({code}){}: {}",
                    match code {
                        401 | 403 => " — check STRAPI_API_TOKEN has `find` on this collection",
                        _ => "",
                    },
                    detail.trim()
                );
            }
            Err(err) => return Err(err).with_context(|| format!("listing {label}s from strapi")),
        };

        let rows = body["data"].as_array().cloned().unwrap_or_default();
        let total = body["meta"]["pagination"]["total"].as_i64().unwrap_or(0);
        if total > rows.len() as i64 {
            eprintln!(
                "stream-recorder: strapi has {total} {label}s but only the first {} are listed",
                rows.len()
            );
        }
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(Entry {
                    id: row["id"].as_i64()?,
                    name: row["name"].as_str()?.trim().to_string(),
                    slug: row["slug"].as_str().unwrap_or_default().to_string(),
                    detail: row[detail_field]
                        .as_str()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty()),
                })
            })
            .collect())
    }

    /// `name` then `slug`, case-insensitively. `None` only when both miss.
    fn find_relation(&self, path: &str, label: &str, name: &str) -> Option<i64> {
        if name.trim().is_empty() {
            return None;
        }
        for field in ["name", "slug"] {
            let url = format!("{}{path}", self.base);
            let response = ureq::get(&url)
                .set("Authorization", &self.bearer())
                .query(&format!("filters[{field}][$eqi]"), name)
                .query("pagination[pageSize]", "1")
                .timeout(LOOKUP_TIMEOUT)
                .call();
            let body: serde_json::Value = match response {
                Ok(response) => match response.into_json() {
                    Ok(body) => body,
                    Err(err) => {
                        eprintln!("stream-recorder: strapi {label} lookup unreadable: {err}");
                        continue;
                    }
                },
                Err(ureq::Error::Status(code, response)) => {
                    let detail = response.into_string().unwrap_or_default();
                    eprintln!(
                        "stream-recorder: strapi {label} lookup by {field} failed ({code}): {}",
                        detail.trim()
                    );
                    continue;
                }
                Err(err) => {
                    eprintln!("stream-recorder: strapi {label} lookup by {field}: {err}");
                    continue;
                }
            };
            if let Some(id) = body["data"].get(0).and_then(|row| row["id"].as_i64()) {
                eprintln!("stream-recorder: strapi matched {label} {field}={name:?} → id={id}");
                return Some(id);
            }
        }
        eprintln!("stream-recorder: strapi found no {label} named {name:?}");
        None
    }

    // --------------------------------------------------------------- media

    /// Uploads an image and returns its media id.
    pub fn upload_media(&self, path: &Path, alt: Option<&str>) -> Result<i64> {
        self.upload(path, alt).map(|uploaded| uploaded.id)
    }

    /// The same upload, keeping the URL as well — what a figure needs.
    pub fn upload_figure(&self, path: &Path, alt: Option<&str>) -> Result<Uploaded> {
        self.upload(path, alt)
    }

    /// A video file, for a cut that is not hosted anywhere else.
    ///
    /// Guarded by size where images are not, and the guard is the point: this
    /// path reads the whole file into memory and then builds a multipart body
    /// around it, so a long cut costs twice its own size in RAM before a single
    /// byte is sent. A figure is a screenshot; this can be a talk.
    pub fn upload_video(&self, path: &Path) -> Result<Uploaded> {
        let size = std::fs::metadata(path)
            .with_context(|| format!("reading {}", path.display()))?
            .len();
        if size > VIDEO_UPLOAD_MAX {
            bail!(
                "{} is {} MB, past the {} MB this uploads — host it and set the URL by hand",
                path.display(),
                size / 1_048_576,
                VIDEO_UPLOAD_MAX / 1_048_576
            );
        }
        self.upload(path, None)
    }

    fn upload(&self, path: &Path, alt: Option<&str>) -> Result<Uploaded> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "thumbnail.jpg".to_string());
        let boundary = boundary_for(&bytes);
        let body = multipart(&boundary, &filename, mime_for(path), &bytes, alt);

        let url = format!("{}{UPLOAD_PATH}", self.base);
        let response = ureq::post(&url)
            .set("Authorization", &self.bearer())
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .timeout(UPLOAD_TIMEOUT)
            .send_bytes(&body);
        let parsed: serde_json::Value = match response {
            Ok(response) => response
                .into_json()
                .context("parsing the upload response")?,
            Err(ureq::Error::Status(code, response)) => {
                let detail = response.into_string().unwrap_or_default();
                bail!("strapi refused the thumbnail ({code}): {}", detail.trim());
            }
            Err(err) => return Err(err).context("uploading the thumbnail to strapi"),
        };
        // `/api/upload` answers with an array, one entry per file sent.
        let (id, url) = media_entry(&parsed).context("strapi accepted the file but named no id")?;
        Ok(Uploaded {
            id,
            url: self.absolute(&url),
        })
    }

    /// A media URL made absolute against this CMS.
    ///
    /// Strapi Cloud answers with an absolute URL already; a self-hosted install
    /// answers with a path. Left alone when it is already absolute rather than
    /// concatenated blindly, which would produce
    /// `https://cms…https://cms…/uploads/…`.
    fn absolute(&self, url: &str) -> String {
        match url.starts_with("http://") || url.starts_with("https://") {
            true => url.to_string(),
            false => format!("{}{}", self.base, url),
        }
    }

    // -------------------------------------------------------------- create

    /// Creates the entry, verifies its relations, and publishes it.
    pub fn create_video_post(&self, post: &NewVideoPost, publish: bool) -> Result<Created> {
        let url = format!("{}{VIDEO_POSTS_PATH}", self.base);
        let mut request = ureq::post(&url)
            .set("Authorization", &self.bearer())
            .set("Content-Type", "application/json")
            .timeout(UPLOAD_TIMEOUT);
        if publish {
            request = request.query(STATUS, PUBLISHED);
        }
        // Ask for every relation back, so a silent drop is visible.
        for (index, field) in RELATIONS.iter().enumerate() {
            request = request.query(&format!("populate[{index}]"), field);
        }
        let (body, dropped) = send_create(request, post.body())?;

        let entry = &body["data"];
        let document_id = entry["documentId"]
            .as_str()
            .context("strapi created the entry but named no documentId")?
            .to_string();
        let entry_id = entry["id"].as_i64().unwrap_or_default();
        let slug = entry["slug"]
            .as_str()
            .unwrap_or(&post.article.slug)
            .to_string();

        let published = is_published(entry);

        let mut warnings = Vec::new();
        warnings.extend(compat::warning(&dropped));
        for (field, sent) in post.relations() {
            if entry[field].is_null() || entry.get(field).is_none() {
                eprintln!(
                    "stream-recorder: strapi echoed {field}=null after create — retrying via PUT"
                );
                if !self.update_relation(&document_id, field, sent, publish) {
                    warnings.push(format!("{field} could not be set"));
                }
            }
        }

        if publish && !published {
            warnings.push("created as a draft — strapi did not publish it".to_string());
        }

        Ok(Created {
            document_id: document_id.clone(),
            entry_id,
            // `/blog/{slug}`, not `/education/{slug}`: the old route is now a
            // permanent redirect, so the legacy form still resolves — but it is
            // the ledger's only record of where the post lives and the URL the
            // pane offers to open, and both should name the page itself.
            public_url: format!("{}/blog/{slug}", public_base()),
            admin_url: admin_url(&self.base, &document_id),
            slug,
            published,
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        })
    }

    /// The retry for a relation that did not stick on create. `true` only when
    /// the `PUT` response echoes it back set.
    fn update_relation(&self, document_id: &str, field: &str, id: i64, publish: bool) -> bool {
        let url = format!("{}{VIDEO_POSTS_PATH}/{document_id}", self.base);
        let mut request = ureq::put(&url)
            .set("Authorization", &self.bearer())
            .set("Content-Type", "application/json")
            .query("populate", field)
            .timeout(LOOKUP_TIMEOUT);
        if publish {
            // Without this the fix lands on the draft only, and the live page
            // keeps the relation it was missing.
            request = request.query(STATUS, PUBLISHED);
        }
        let response = request.send_json(serde_json::json!({ "data": { field: id } }));
        match response {
            Ok(response) => match response.into_json::<serde_json::Value>() {
                Ok(body) => !body["data"][field].is_null(),
                Err(_) => false,
            },
            Err(ureq::Error::Status(code, response)) => {
                let detail = response.into_string().unwrap_or_default();
                eprintln!(
                    "stream-recorder: strapi {field} PUT failed ({code}): {}",
                    detail.trim()
                );
                false
            }
            Err(err) => {
                eprintln!("stream-recorder: strapi {field} PUT: {err}");
                false
            }
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn admin_url(base: &str, document_id: &str) -> String {
    format!(
        "{base}/admin/content-manager/collection-types/api::video-post.video-post/{document_id}"
    )
}

/// Retry only a known pre-create validation rejection, never ambiguous transport
/// failures (where another POST could duplicate a successfully created document).
///
/// The rejection worth retrying is `Invalid key <field>`: the instance is older
/// than the schema this writes against and does not have that column. Strapi
/// names one key per refusal, so this loops — dropping the field it named and
/// sending again — and returns everything it had to give up. See [`compat`].
///
/// Safe to repeat because the refusal happens in validation, before the
/// document is created: a refused POST leaves nothing behind to duplicate.
fn send_create(
    request: ureq::Request,
    mut payload: serde_json::Value,
) -> Result<(serde_json::Value, Vec<String>)> {
    let mut dropped: Vec<String> = Vec::new();
    loop {
        match request.clone().send_json(payload.clone()) {
            Ok(response) => {
                let body = response
                    .into_json()
                    .context("parsing the create response")?;
                return Ok((body, dropped));
            }
            Err(ureq::Error::Status(code, response)) => {
                let detail = response.into_string().unwrap_or_default();
                let fixable = (dropped.len() < compat::MAX_DROPPED)
                    .then(|| compat::unknown_key(code, &detail))
                    .flatten()
                    .filter(|key| compat::drop_field(&mut payload, key, &dropped));
                if let Some(key) = fixable {
                    eprintln!("stream-recorder: strapi has no {key} — retrying without it");
                    dropped.push(key);
                    continue;
                }
                bail!("strapi refused the video post ({code}): {}", detail.trim());
            }
            Err(err) => return Err(err).context("creating the video post"),
        }
    }
}

/// Whether the document Strapi handed back is live.
///
/// Read off `publishedAt` rather than assumed from the flag that was sent: the
/// request asking to publish and the document actually being published are two
/// different facts, and the whole reason this code was wrong before is that it
/// treated the first as proof of the second.
fn is_published(entry: &serde_json::Value) -> bool {
    entry["publishedAt"]
        .as_str()
        .is_some_and(|at| !at.trim().is_empty())
}

/// The id and URL out of `/api/upload`'s array-shaped answer.
fn media_entry(body: &serde_json::Value) -> Option<(i64, String)> {
    let first = body.as_array()?.first()?;
    let id = first["id"].as_i64()?;
    // A missing URL is not fatal for the thumbnail, which only wants the id, so
    // this is empty rather than `None` — the figure path checks it instead.
    let url = first["url"].as_str().unwrap_or_default().to_string();
    Some((id, url))
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// A boundary that cannot occur inside the body.
///
/// Derived from the bytes rather than a random number so a retry sends an
/// identical request, and then checked against them — a boundary that appears in
/// the payload would truncate the upload at that point rather than fail.
fn boundary_for(bytes: &[u8]) -> String {
    let mut boundary = format!(
        "streamrecorder{}",
        crate::agent::prompt::hash_of_bytes(bytes)
    );
    while contains(bytes, boundary.as_bytes()) {
        boundary.push('x');
    }
    boundary
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// `multipart/form-data` with Strapi's two field names: `files` for the image and
/// an optional JSON-encoded `fileInfo` for its alt text.
fn multipart(
    boundary: &str,
    filename: &str,
    mime: &str,
    bytes: &[u8],
    alt: Option<&str>,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + 512);
    if let Some(alt) = alt.map(str::trim).filter(|alt| !alt.is_empty()) {
        let info = serde_json::json!({ "alternativeText": alt }).to_string();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"fileInfo\"\r\n\r\n{info}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; \
             filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_multipart_body_names_both_fields_strapi_reads() {
        let body = multipart(
            "BOUND",
            "thumb.jpg",
            "image/jpeg",
            b"\xff\xd8jpeg",
            Some("A cover"),
        );
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"fileInfo\""));
        assert!(text.contains(r#"{"alternativeText":"A cover"}"#));
        assert!(text.contains("name=\"files\"; filename=\"thumb.jpg\""));
        assert!(text.contains("Content-Type: image/jpeg"));
        assert!(text.ends_with("\r\n--BOUND--\r\n"));
    }

    /// The bytes must survive verbatim — a jpeg is not valid UTF-8, so any
    /// string round-trip in the builder would corrupt it.
    #[test]
    fn the_image_bytes_are_carried_untouched() {
        let raw: Vec<u8> = vec![0xff, 0xd8, 0x00, 0x1f, 0xfe];
        let body = multipart("BOUND", "t.jpg", "image/jpeg", &raw, None);
        assert!(contains(&body, &raw));
        assert!(!String::from_utf8_lossy(&body).contains("fileInfo"));
    }

    /// A blank alt text is no alt text — sending `""` would set the field to
    /// empty rather than leaving it for someone to fill in.
    #[test]
    fn a_blank_alt_text_is_omitted() {
        let body = multipart("BOUND", "t.jpg", "image/jpeg", b"x", Some("   "));
        assert!(!String::from_utf8_lossy(&body).contains("fileInfo"));
    }

    /// A boundary occurring inside the image would truncate the upload there.
    #[test]
    fn a_boundary_never_occurs_inside_the_body_it_delimits() {
        let bytes = b"some jpeg bytes".to_vec();
        let boundary = boundary_for(&bytes);
        assert!(!contains(&bytes, boundary.as_bytes()));
        // Derived from the content, so a retry sends the identical request.
        assert_eq!(boundary, boundary_for(&bytes));
        assert_ne!(boundary, boundary_for(b"different"));
    }

    /// Contrived, but it is the branch that stops a truncated upload.
    #[test]
    fn a_colliding_boundary_is_extended_until_it_is_unique() {
        let seed = b"payload".to_vec();
        let natural = boundary_for(&seed);
        let mut poisoned = seed.clone();
        poisoned.extend_from_slice(natural.as_bytes());
        let extended = boundary_for(&poisoned);
        assert!(extended.len() > natural.len() || extended != natural);
        assert!(!contains(&poisoned, extended.as_bytes()));
    }

    #[test]
    fn the_media_id_and_url_come_out_of_the_array_strapi_answers_with() {
        let body = serde_json::json!([{ "id": 42, "url": "/uploads/t.jpg" }]);
        assert_eq!(media_entry(&body), Some((42, "/uploads/t.jpg".to_string())));
        assert_eq!(media_entry(&serde_json::json!([])), None);
        assert_eq!(media_entry(&serde_json::json!({ "id": 42 })), None);
        // The thumbnail only wants the id, so a missing URL is the figure
        // path's problem to notice rather than a parse failure here.
        assert_eq!(
            media_entry(&serde_json::json!([{ "id": 7 }])),
            Some((7, String::new()))
        );
    }

    /// Strapi Cloud answers absolute and a self-hosted install answers with a
    /// path. Concatenating blindly would produce `https://cms…https://cms…`.
    #[test]
    fn a_media_url_is_made_absolute_only_when_it_is_not_already() {
        let client = Strapi {
            base: "https://cms.saagasolve.com".into(),
            token: "t".into(),
        };
        assert_eq!(
            client.absolute("/uploads/figure_01.jpg"),
            "https://cms.saagasolve.com/uploads/figure_01.jpg"
        );
        for already in [
            "https://media.strapiapp.com/figure_01.jpg",
            "http://localhost:1337/uploads/figure_01.jpg",
        ] {
            assert_eq!(client.absolute(already), already);
        }
    }

    #[test]
    fn mime_types_follow_the_extension() {
        assert_eq!(mime_for(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(mime_for(Path::new("a.JPEG")), "image/jpeg");
        assert_eq!(mime_for(Path::new("a.png")), "image/png");
        assert_eq!(mime_for(Path::new("a.bin")), "application/octet-stream");
    }

    /// The admin URL is what a bad post is fixed through, so it has to point at
    /// the entry rather than the collection.
    /// A draft comes back with `publishedAt: null`, and that is the only signal
    /// that the `?status=published` write did not take.
    #[test]
    fn published_is_read_off_the_document_not_the_request() {
        let live = serde_json::json!({ "publishedAt": "2026-08-29T20:00:00.000Z" });
        assert!(is_published(&live));

        for draft in [
            serde_json::json!({ "publishedAt": serde_json::Value::Null }),
            serde_json::json!({ "publishedAt": "" }),
            serde_json::json!({}),
        ] {
            assert!(!is_published(&draft), "{draft}");
        }
    }

    #[test]
    fn the_admin_url_points_at_the_entry() {
        let got = admin_url("https://cms.saagasolve.com", "abc123");
        assert!(got.ends_with("api::video-post.video-post/abc123"));
        assert!(got.starts_with("https://cms.saagasolve.com/admin/"));
    }

    #[test]
    fn the_public_base_has_no_trailing_slash() {
        assert!(!public_base().ends_with('/'));
    }
}
