/* Embedded inside each template's IIFE; no globals or extra timelines.
   Selectors are passed explicitly so instances keep their own scoped targets. */
function addChapterMotion(tl, options) {
  var elements = [options.meta, options.title, options.signature];
  if (options.animateIn) {
    // Even a one-second card completes its entrance with time left to read.
    var beat = Math.min(1, options.duration / 1.8);
    tl.fromTo(options.meta,
      { autoAlpha: 0, y: 14 },
      { autoAlpha: 1, y: 0, duration: 0.36 * beat, ease: 'power3.out' },
      0.04 * beat);
    // The rule sits between the number and the title, so it draws first and
    // the title rises in beneath it.
    tl.fromTo(options.signature,
      { autoAlpha: 0, y: 0 },
      { autoAlpha: 1, y: 0, duration: 0.3 * beat, ease: 'power2.out' },
      0.12 * beat);
    tl.fromTo(options.rule,
      { scaleX: 0 },
      { scaleX: 1, duration: 0.48 * beat, ease: 'power3.out' },
      0.12 * beat);
    tl.fromTo(options.title,
      { autoAlpha: 0, y: 28 },
      { autoAlpha: 1, y: 0, duration: 0.5 * beat, ease: 'power3.out' },
      0.2 * beat);
  } else {
    // Footage openers and holdFromStart cards remain readable in frame zero.
    tl.set(elements, { autoAlpha: 1, y: 0 }, 0);
    tl.set(options.rule, { scaleX: 1 }, 0);
  }
  if (typeof options.exitAt === 'number') {
    tl.fromTo(options.plate,
      { autoAlpha: 1, y: 0 },
      { autoAlpha: 0, y: -24, duration: options.exitDuration,
        ease: 'power2.in', immediateRender: false },
      options.exitAt);
  }
}
