# Vendored pane libraries

Checked in rather than fetched, because a webview pane is loaded from a `file://`
page with no base URL — see `ui::web::WebPane::show_local`. There is nothing for a
`<script src>` to resolve against, a CDN would make the editor need the network, and
an ESM build would be refused by CORS at a `file://` origin. Both files below are
**UMD**, so `include_str!` can inline them into the page and they attach to
`window.WaveSurfer`.

| file | package | version | license |
|---|---|---|---|
| `wavesurfer.min.js` | [wavesurfer.js](https://wavesurfer.xyz) | 7.12.11 | BSD-3-Clause |
| `wavesurfer-regions.min.js` | wavesurfer.js Regions plugin | 7.12.11 | BSD-3-Clause |

Used by `ui/templates/edit.html`: the waveform is drawn from peaks that
`edit::waveform` computed in Rust, and the Regions plugin is what makes a keep-span
something you can drag out over the audio.

To refresh — keep both files on the same version, the plugin reaches into
wavesurfer's internals:

```sh
V=7.12.11
curl -sL "https://unpkg.com/wavesurfer.js@$V/dist/wavesurfer.min.js" \
  -o src/ui/vendor/wavesurfer.min.js
curl -sL "https://unpkg.com/wavesurfer.js@$V/dist/plugins/regions.min.js" \
  -o src/ui/vendor/wavesurfer-regions.min.js
```

Then check `ui::render`'s test that the bundle is still inlined, and that the file
still starts with the UMD preamble (`!function(t,e){"object"==typeof exports…`)
rather than an `import` — upstream switching the default build to ESM is the one
change that would break this silently.
