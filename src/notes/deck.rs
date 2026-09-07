use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub points: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbatim: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotesData {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub chapters: Vec<Chapter>,
}

pub const NOTES_JSON: &str = "notes.json";
pub const NOTES_HTML: &str = "notes.html";

pub fn write(dir: &Path, data: &NotesData) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let json_path = dir.join(NOTES_JSON);
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(data).context("serializing notes")? + "\n",
    )
    .with_context(|| format!("writing {}", json_path.display()))?;
    let html_path = dir.join(NOTES_HTML);
    std::fs::write(&html_path, render(data))
        .with_context(|| format!("writing {}", html_path.display()))?;
    Ok(html_path)
}

pub fn load(dir: &Path) -> Result<NotesData> {
    let json_path = dir.join(NOTES_JSON);
    let text = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading {}", json_path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", json_path.display()))
}

pub fn copy_into(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for name in [NOTES_JSON, NOTES_HTML] {
        let src = from.join(name);
        if src.exists() {
            std::fs::copy(&src, to.join(name))
                .with_context(|| format!("copying {} to {}", src.display(), to.display()))?;
        }
    }
    Ok(())
}

pub fn html_path(dir: &Path) -> PathBuf {
    dir.join(NOTES_HTML)
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str(concat!("&", "amp;")),
            '<' => out.push_str(concat!("&", "lt;")),
            '>' => out.push_str(concat!("&", "gt;")),
            '"' => out.push_str(concat!("&", "quot;")),
            _ => out.push(c),
        }
    }
    out
}

fn json_for_script(data: &NotesData) -> String {
    serde_json::to_string(data)
        .unwrap_or_else(|_| "{}".into())
        .replace('<', "\\u003c")
}

pub fn render(data: &NotesData) -> String {
    let total = data.chapters.len().max(1);
    let slides: String = if data.chapters.is_empty() {
        "<section class=\"slide active\"><h1>No chapters</h1><ul class=points><li>Notes produced nothing.</li></ul></section>".into()
    } else {
        data.chapters
            .iter()
            .enumerate()
            .map(|(n, chapter)| {
                let points = chapter
                    .points
                    .iter()
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| format!("<li>{}</li>", esc(p)))
                    .collect::<String>();
                let verbatim = chapter
                    .verbatim
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(esc)
                    .unwrap_or_default();
                let cues = chapter
                    .cues
                    .iter()
                    .filter(|c| !c.trim().is_empty())
                    .map(|c| format!("<span>{}</span>", esc(c)))
                    .collect::<String>();
                format!(
                    "<section class=slide data-index=\"{n}\">\
                     <header><span class=chapter>Chapter {num}</span>\
                     <span class=count>{num} / {total}</span></header>\
                     <h1>{title}</h1>\
                     <ul class=points>{points}</ul>\
                     <blockquote class=verbatim><span class=label>say exactly</span>\
                     <span class=text>{verbatim}</span></blockquote>\
                     <div class=cues>{cues}</div></section>",
                    num = n + 1,
                    title = esc(&chapter.title),
                )
            })
            .collect()
    };
    let title = esc(&data.title);
    let payload = json_for_script(data);
    format!(
        r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} — speaking notes</title>
<style>
*{{margin:0;padding:0;box-sizing:border-box}}
:root{{--bg:#0b0d10;--fg:#f4f6f8;--dim:#8b94a0;--accent:#6ea8fe;--rule:#1e242c}}
html,body{{height:100%;background:var(--bg);color:var(--fg);
  font:400 15px/1.4 -apple-system,BlinkMacSystemFont,system-ui,sans-serif}}
body{{display:flex;align-items:center;justify-content:center;overflow:hidden;padding:16px}}
.slide{{display:none;width:100%;max-height:100%;overflow:auto}}
.slide.active{{display:block}}
header{{display:flex;justify-content:space-between;border-bottom:1px solid var(--rule);
  padding-bottom:8px;margin-bottom:16px}}
.chapter{{color:var(--accent);font-size:11px;letter-spacing:.14em;text-transform:uppercase;font-weight:600}}
.count{{color:var(--dim);font-size:12px}}
h1{{font-size:28px;line-height:1.1;font-weight:700;margin-bottom:16px}}
.points{{list-style:none}}
.points li{{font-size:18px;line-height:1.3;font-weight:500;padding-left:1.2em;text-indent:-1.2em;margin-bottom:10px}}
.points li::before{{content:"—";color:var(--accent);padding-right:.4em}}
.verbatim{{margin-top:16px;padding:12px 14px;border-left:4px solid var(--accent);background:#12161b;font-size:16px;font-weight:600}}
.verbatim .label{{display:block;color:var(--accent);font-size:10px;letter-spacing:.16em;text-transform:uppercase;margin-bottom:6px}}
.verbatim:has(.text:empty){{display:none}}
.cues{{margin-top:14px;display:flex;flex-wrap:wrap;gap:6px}}
.cues:empty{{display:none}}
.cues span{{color:var(--dim);border:1px solid var(--rule);border-radius:999px;padding:3px 10px;font-size:12px}}
#bar{{position:fixed;top:0;left:0;height:3px;background:var(--accent)}}
</style></head><body>
<div id="bar"></div>
{slides}
<script id="notes-data" type="application/json">{payload}</script>
<script>
const slides=[...document.querySelectorAll(".slide")];
const bar=document.getElementById("bar");
let i=0;
function show(n){{
  i=Math.max(0,Math.min(slides.length-1,n));
  slides.forEach((s,k)=>s.classList.toggle("active",k===i));
  bar.style.width=((i+1)/Math.max(slides.length,1))*100+"%";
}}
window.nextSlide=function(){{show(i+1)}};
window.prevSlide=function(){{show(i-1)}};
window.goToSlide=function(n){{show(n)}};
window.currentSlide=function(){{return i}};
document.addEventListener("click",function(){{nextSlide();}});
document.addEventListener("keydown",function(e){{
  if(e.key==="ArrowRight"||e.key===" "||e.key==="PageDown"){{e.preventDefault();nextSlide();}}
  if(e.key==="ArrowLeft"||e.key==="PageUp"){{e.preventDefault();prevSlide();}}
  if(e.key==="Home"){{e.preventDefault();goToSlide(0);}}
}});
show(0);
</script></body></html>
"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NotesData {
        NotesData {
            title: "Hook <test>".into(),
            version: Some(1),
            chapters: vec![
                Chapter {
                    title: "The hook".into(),
                    points: vec!["open with the problem".into()],
                    verbatim: Some("Most teams ship this wrong.".into()),
                    cues: vec!["slow down".into()],
                },
                Chapter {
                    title: "The fix".into(),
                    points: vec!["one command".into()],
                    verbatim: None,
                    cues: vec![],
                },
            ],
        }
    }

    #[test]
    fn render_escapes_the_title_and_exposes_next_slide() {
        let html = render(&sample());
        assert!(html.contains(&format!("Hook {}", concat!("&", "lt;", "test", "&", "gt;"))));
        assert!(html.contains("window.nextSlide"));
        assert!(html.contains("The hook"));
        assert!(html.contains("The fix"));
        assert!(!html.contains("<test>"));
    }

    #[test]
    fn write_and_copy_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-deck-{}-{}",
            std::process::id(),
            "write"
        ));
        let dest = dir.join("copy");
        let _ = std::fs::remove_dir_all(&dir);
        let html = write(&dir, &sample()).expect("write");
        assert!(html.exists());
        copy_into(&dir, &dest).expect("copy");
        assert!(dest.join(NOTES_HTML).exists());
        let back = load(&dir).expect("load");
        assert_eq!(back.chapters.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
