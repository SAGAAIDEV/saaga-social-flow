//! Add the completed YouTube/blog work to social copy without exposing draft links.
use super::generate::VideoContext;
use crate::session::Session;

pub fn enrich(session: &Session, videos: &mut [VideoContext]) {
    let uploads = crate::publish::load(session);
    let latest = uploads.last();
    let article = crate::blog::schema::load(&session.blog_dir()).ok();
    let blog_posts = crate::blog::load(session);
    let published_blog = latest
        .and_then(|upload| {
            blog_posts
                .iter()
                .rev()
                .find(|post| post.video_id == upload.video_id)
        })
        .filter(|post| post.published);
    for video in videos {
        if let Some(upload) =
            latest.filter(|upload| upload.privacy == crate::publish::youtube::Privacy::Public)
        {
            video.points.push(format!(
                "Published full video: {} — {}",
                upload.title, upload.url
            ));
        }
        if let Some(post) = published_blog {
            video.points.push(format!(
                "Published article (use this exact URL when linking): {}",
                post.url
            ));
        }
        if video.id == "longform" {
            if let Some(article) = &article {
                video.points.push(format!(
                    "Companion article: {}\n{}",
                    article.title, article.description
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_videos_and_draft_articles_do_not_become_promotional_links() {
        let root = std::env::temp_dir().join(format!("social-publication-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let session = Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        };
        let upload = serde_json::json!({"video_id":"abc", "url":"https://youtube.com/watch?v=abc", "title":"Video", "source_hash":"hash", "uploaded_at":"now", "privacy":"private"});
        std::fs::write(root.join(crate::publish::UPLOADS_JSONL), upload.to_string()).unwrap();
        let blog = serde_json::json!({"document_id":"doc", "slug":"article", "url":"https://example.com/blog/article", "admin_url":"https://cms.example.com/draft", "video_id":"abc", "published":false, "created_at":"now"});
        std::fs::write(root.join(crate::blog::POSTS_JSONL), blog.to_string()).unwrap();
        let mut videos = vec![VideoContext {
            id: "longform".into(),
            video_type: "horizontal".into(),
            title: "Video".into(),
            points: vec![],
            transcript_text: String::new(),
        }];
        enrich(&session, &mut videos);
        assert!(videos[0].points.is_empty());
        let mut public = upload;
        public["privacy"] = serde_json::json!("public");
        std::fs::write(root.join(crate::publish::UPLOADS_JSONL), public.to_string()).unwrap();
        enrich(&session, &mut videos);
        assert!(videos[0].points[0].contains("watch?v=abc"));
        assert!(!videos[0]
            .points
            .iter()
            .any(|point| point.contains("example.com")));
        let mut published_blog = blog;
        published_blog["published"] = serde_json::json!(true);
        std::fs::write(
            root.join(crate::blog::POSTS_JSONL),
            published_blog.to_string(),
        )
        .unwrap();
        videos[0].points.clear();
        enrich(&session, &mut videos);
        assert!(videos[0]
            .points
            .iter()
            .any(|point| point.contains("https://example.com/blog/article")));
        assert!(!videos[0]
            .points
            .iter()
            .any(|point| point.contains("cms.example.com")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
