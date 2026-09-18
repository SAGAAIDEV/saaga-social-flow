/* A small, reusable controller for static decks. Slide and point state are
   explicit, so Back, direct links and rapid input restore the same content. */
(() => {
  const deck = document.querySelector(".deck");
  const stage = deck.querySelector(".stage");
  const slides = [...stage.querySelectorAll(".slide")];
  const chapterNav = deck.querySelector(".chapter-nav");
  const pointNav = deck.querySelector(".point-dots");
  const back = deck.querySelector(".back");
  const next = deck.querySelector(".next");
  const notes = deck.querySelector(".notes");
  const notesToggle = deck.querySelector(".notes-toggle");
  const announcement = document.querySelector("#announcement");
  const counts = slides.map(slide =>
    Math.max(0, ...[...slide.querySelectorAll("[data-step]")].map(el => Number(el.dataset.step))) + 1
  );
  const portrait = matchMedia("(max-aspect-ratio: 4/5)");
  let slideIndex = 0;
  let pointIndex = 0;
  let renderedSlide = -1;

  const chapterButtons = slides.map((slide, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "chapter";
    button.setAttribute("aria-label", "Slide " + (index + 1) + ": " + slide.dataset.label);
    const number = document.createElement("span");
    number.textContent = String(index + 1).padStart(2, "0");
    button.append(number, document.createTextNode(slide.dataset.label));
    button.addEventListener("click", () => go(index, 0));
    chapterNav.append(button);
    return button;
  });

  function resize() {
    const width = portrait.matches ? 900 : 1600;
    const height = portrait.matches ? 1440 : 900;
    const scale = Math.min(document.documentElement.clientWidth / width, innerHeight / height);
    deck.style.setProperty("--deck-scale", scale);
  }

  function go(newSlide, newPoint, updateHash = true) {
    const boundedSlide = Math.max(0, Math.min(slides.length - 1, newSlide));
    const boundedPoint = Math.max(0, Math.min(counts[boundedSlide] - 1, newPoint));
    deck.classList.toggle("reversing", boundedSlide < slideIndex ||
      (boundedSlide === slideIndex && boundedPoint < pointIndex));
    slideIndex = boundedSlide;
    pointIndex = boundedPoint;
    const activeSlide = slides[slideIndex];
    deck.dataset.theme = activeSlide.dataset.theme;
    deck.dataset.slide = activeSlide.id;
    deck.dataset.point = String(pointIndex + 1);

    slides.forEach((slide, index) => {
      const active = index === slideIndex;
      slide.classList.toggle("leaving", slide.classList.contains("active") && !active);
      slide.classList.toggle("active", active);
      slide.setAttribute("aria-hidden", String(!active));
      slide.setAttribute("role", "group");
      slide.setAttribute("aria-roledescription", "slide");
      slide.inert = !active;
      slide.querySelectorAll("[data-step]").forEach(fragment => {
        const visible = active && Number(fragment.dataset.step) <= pointIndex;
        fragment.classList.toggle("revealed", visible);
        fragment.classList.toggle("current", active && Number(fragment.dataset.step) === pointIndex);
        fragment.setAttribute("aria-hidden", String(!visible));
      });
    });

    if (renderedSlide !== slideIndex) {
      pointNav.replaceChildren();
      for (let point = 0; point < counts[slideIndex]; point++) {
        const button = document.createElement("button");
        button.type = "button";
        button.setAttribute("aria-label", "Go to point " + (point + 1));
        button.addEventListener("click", () => go(slideIndex, point));
        pointNav.append(button);
      }
      renderedSlide = slideIndex;
    }
    [...pointNav.children].forEach((button, point) => {
      if (point === pointIndex) button.setAttribute("aria-current", "step");
      else button.removeAttribute("aria-current");
    });
    chapterButtons.forEach((button, index) => {
      const fill = index < slideIndex ? 1 : index === slideIndex ? (pointIndex + 1) / counts[index] : 0;
      button.style.setProperty("--fill", fill);
      if (index === slideIndex) button.setAttribute("aria-current", "step");
      else button.removeAttribute("aria-current");
    });
    back.disabled = slideIndex === 0 && pointIndex === 0;
    const isLastPoint = pointIndex === counts[slideIndex] - 1;
    const isEnd = isLastPoint && slideIndex === slides.length - 1;
    const nextLabel = isEnd ? "Replay deck" : isLastPoint ? "Next slide" : "Next point";
    next.querySelector(".next-label").textContent = nextLabel;
    next.setAttribute("aria-label", nextLabel);
    deck.querySelector(".point-count").textContent = (pointIndex + 1) + " / " + counts[slideIndex];
    const note = activeSlide.querySelector('[data-step="' + pointIndex + '"][data-note]');
    notes.querySelector("p").textContent = note?.dataset.note || "";
    const currentPoint = activeSlide.querySelector('.point[data-step="' + pointIndex + '"]');
    announcement.textContent = activeSlide.dataset.label + ". Slide " + (slideIndex + 1) +
      " of " + slides.length + ". Point " + (pointIndex + 1) + " of " + counts[slideIndex] +
      (currentPoint ? ". " + currentPoint.textContent.replace(/^\s*\d+\s*/, "") : "");
    if (updateHash) {
      const hash = "#" + activeSlide.id + "/" + (pointIndex + 1);
      try { history.replaceState(null, "", hash); }
      catch { if (location.hash !== hash) location.hash = hash; }
    }
  }

  function advance() {
    if (pointIndex + 1 < counts[slideIndex]) go(slideIndex, pointIndex + 1);
    else if (slideIndex + 1 < slides.length) go(slideIndex + 1, 0);
    // The final slide holds. Restarting is an explicit Replay button action.
  }

  function previous() {
    if (pointIndex > 0) go(slideIndex, pointIndex - 1);
    else if (slideIndex > 0) go(slideIndex - 1, counts[slideIndex - 1] - 1);
  }

  function restoreHash() {
    const match = location.hash.match(/^#([a-z-]+)(?:\/(\d+))?$/);
    const index = match ? slides.findIndex(slide => slide.id === match[1]) : -1;
    go(index < 0 ? 0 : index, index < 0 ? 0 : Math.max(0, Number(match[2] || 1) - 1));
  }

  function toggleNotes() {
    notes.hidden = !notes.hidden;
    notesToggle.setAttribute("aria-expanded", String(!notes.hidden));
  }

  back.addEventListener("click", previous);
  next.addEventListener("click", () => {
    if (slideIndex === slides.length - 1 && pointIndex === counts[slideIndex] - 1) go(0, 0);
    else advance();
  });
  notesToggle.addEventListener("click", toggleNotes);
  stage.addEventListener("click", event => {
    if (event.target.closest("button,a,input,textarea,select") || getSelection()?.toString()) return;
    advance();
  });
  document.addEventListener("keydown", event => {
    if (event.altKey || event.ctrlKey || event.metaKey ||
        event.target.closest("input,textarea,select,[contenteditable=true]")) return;
    if (event.key === " " && event.target.closest("button,a")) return;
    if (["ArrowRight", "PageDown", " "].includes(event.key)) {
      event.preventDefault();
      advance();
    } else if (["ArrowLeft", "PageUp"].includes(event.key)) {
      event.preventDefault();
      previous();
    } else if (event.key === "Home") {
      event.preventDefault();
      go(0, 0);
    } else if (event.key === "End") {
      event.preventDefault();
      go(slides.length - 1, counts.at(-1) - 1);
    } else if (event.key.toLowerCase() === "n" && !event.repeat) {
      toggleNotes();
    } else if (event.key === "Escape") {
      notes.hidden = true;
      notesToggle.setAttribute("aria-expanded", "false");
    }
  });
  addEventListener("resize", resize);
  portrait.addEventListener("change", resize);
  addEventListener("hashchange", restoreHash);
  resize();
  restoreHash();
})();
