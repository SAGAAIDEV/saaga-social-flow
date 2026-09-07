//! WKWebView pane that shows `notes.html`.

use std::path::Path;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};

const PLACEHOLDER: &str = "<!doctype html><html><body style=\"margin:0;background:#0b0d10;color:#8b94a0;font:15px/1.4 -apple-system,system-ui,sans-serif;padding:24px\">No notes yet. Record a take, then press Notes.</body></html>";

const NEXT_SLIDE_JS: &str = r#"(function(){
  if (typeof nextSlide === "function") { nextSlide(); return; }
  var slides = Array.prototype.slice.call(document.querySelectorAll(".slide"));
  if (!slides.length) return;
  var cur = slides.findIndex(function(s){ return s.classList.contains("active"); });
  var n = Math.min(slides.length - 1, Math.max(0, cur + 1));
  slides.forEach(function(s, k){ s.classList.toggle("active", k === n); });
})()"#;

const RESET_SLIDE_JS: &str = r#"(function(){
  if (typeof goToSlide === "function") { goToSlide(0); return; }
  var slides = Array.prototype.slice.call(document.querySelectorAll(".slide"));
  slides.forEach(function(s, k){ s.classList.toggle("active", k === 0); });
})()"#;

pub struct NotesPane {
    webview: Retained<WKWebView>,
}

impl NotesPane {
    pub fn attach(parent: &NSView, mtm: MainThreadMarker) -> NotesPane {
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        let webview = unsafe {
            WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
                &config,
            )
        };
        parent.addSubview(&webview);
        let pane = NotesPane { webview };
        pane.show_placeholder();
        pane
    }

    pub fn set_frame(&self, frame: objc2_foundation::NSRect) {
        self.webview.setFrame(frame);
    }

    pub fn show_placeholder(&self) {
        self.show_html(PLACEHOLDER);
    }

    pub fn show_status(&self, message: &str) {
        let mut escaped = String::new();
        for c in message.chars() {
            match c {
                '&' => escaped.push_str(concat!("&", "amp;")),
                '<' => escaped.push_str(concat!("&", "lt;")),
                '>' => escaped.push_str(concat!("&", "gt;")),
                _ => escaped.push(c),
            }
        }
        self.show_html(&format!(
            "<!doctype html><html><body style=\"margin:0;background:#0b0d10;color:#8b94a0;font:15px/1.4 -apple-system,system-ui,sans-serif;padding:24px\">{escaped}</body></html>"
        ));
    }

    fn show_html(&self, html: &str) {
        unsafe {
            self.webview
                .loadHTMLString_baseURL(&NSString::from_str(html), None);
        }
    }

    pub fn load(&self, html: &Path) {
        match std::fs::read_to_string(html) {
            Ok(text) => self.show_html(&text),
            Err(err) => self.show_status(&format!("Could not read notes: {err}")),
        }
    }

    pub fn next_slide(&self) {
        self.eval(NEXT_SLIDE_JS);
    }

    pub fn reset_slide(&self) {
        self.eval(RESET_SLIDE_JS);
    }

    fn eval(&self, js: &str) {
        unsafe {
            self.webview
                .evaluateJavaScript_completionHandler(&NSString::from_str(js), None);
        }
    }
}
